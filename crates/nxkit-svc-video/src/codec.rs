//! The state of the `nn::codec` video decoders of the Switch 2 SDK.
//!
//! The SDK sends every call of these decoders to a decoder service over IPC that the host does not serve, so the
//! host answers them (R43). The protocol is the one of `dm/host/hle_codec_video.c` of nxnative:
//!
//! - The game makes one image pool per output picture and registers each with the decoder under an index.
//!   Registering puts the index in the queue of buffers that can be acquired (8 deep), with a sync handle that is
//!   already signalled.
//! - Acquire takes the oldest index and its sync handle. Release gives the buffer to the decoder to fill, and puts it
//!   back in the queue at once with a new handle. The handle signals when the decoder has written a picture into the
//!   buffer. The game polls `nn::gs::TryWaitNativeSync` on it or waits on a sync point event.
//! - Submit takes one packet with its time and an end of stream flag. A decoder that cannot take more answers
//!   [`BUSY`], and the game retries after the submission ready event.
//!
//! Pictures go into the released buffers in release order. A picture with no released buffer waits in a short queue,
//! and a full queue refuses packets.

use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use nxkit_abi::guest;

use crate::config;
use crate::decoder::{Backend, decode_av1, decode_packet};
use crate::frame::Frame;

/// Marks a handle that this crate wrote into an SDK object, in the upper 32 bits.
pub(crate) const MAGIC: u32 = 0x5643_6d44;
/// Marks a sync handle that this crate made.
pub(crate) const SYNC_MAGIC: u32 = 0x5343_6d44;
/// The decoder buffers of one decoder.
pub(crate) const BUFS: usize = 32;
/// The buffers that can be acquired at once.
const FIFO: usize = 8;
/// Decoded pictures that wait for a released buffer, at which the submission ready event goes down.
pub(crate) const READY: usize = 4;
/// Decoded pictures that wait for a released buffer, at which packets are refused. A player that sends one packet per
/// signal and ignores the result needs room for a signal that is in flight when the event goes down.
pub(crate) const HOLD: usize = 12;
/// The decoders that exist at once. The sync handle names one by a number from 1 to this.
const MAX_DECODERS: u32 = 16;
/// Where the submission ready event lies inside the decoder object of the SDK.
pub(crate) const EVENT_OFFSET: u64 = 0x100;
/// A picture of a native image sits on a 16 KB page, as a Metal buffer over it needs.
const NATIVE_ALIGN: u64 = 0x4000;
const MAX_POOLS: usize = 256;
const MAX_BLANK: usize = 32;
const MAX_ARMED: usize = 64;
/// The stack of the thread that decodes one packet. A guest stack can be 8 KB.
const DECODE_STACK: usize = 2 << 20;

/// An `nn::codec` result (module 111) with the description `desc`.
pub(crate) const fn nn_result(desc: u32) -> u32 {
    0x6f | (desc << 9)
}

/// The decoder cannot take the packet now.
pub(crate) const BUSY: u32 = nn_result(3603);
/// There is no buffer to acquire, or no picture in the buffer yet.
pub(crate) const NONE: u32 = nn_result(3604);
/// The decoder or the index is not valid.
pub(crate) const BAD: u32 = nn_result(2004);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `NXKIT_DIAG=codec` (or `all`): one line per decoder call, at most 600, to see where a player stops.
pub(crate) fn trace(what: std::fmt::Arguments<'_>) {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU32, Ordering};
    static ON: OnceLock<bool> = OnceLock::new();
    static N: AtomicU32 = AtomicU32::new(0);
    let on = *ON.get_or_init(|| {
        std::env::var("NXKIT_DIAG").is_ok_and(|v| v.split(',').any(|c| c == "codec" || c == "all"))
    });
    if on && N.fetch_add(1, Ordering::Relaxed) < 600 {
        eprintln!("DIAG codec {what}");
    }
}

fn red(what: &str) {
    eprintln!("[RED] video: {what}");
}

/// The codec of a decoder type of the SDK.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Codec {
    Vp8,
    Vp9,
    H264,
    Av1,
}

impl Codec {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Codec::Vp8 => "VP8",
            Codec::Vp9 => "VP9",
            Codec::H264 => "H.264",
            Codec::Av1 => "AV1",
        }
    }
}

/// `nn::gs::NativeSyncHandle`, 16 bytes that the game passes by value.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SyncHandle {
    pub(crate) magic: u32,
    pub(crate) dec: u32,
    pub(crate) idx: u32,
    pub(crate) generation: u32,
}

/// `VideoDecoderImagePreference` as the host fills it. It comes back in memory (`x8`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Pref {
    pub(crate) w: u16,
    pub(crate) h: u16,
    pub(crate) fmt: u32,
    pub(crate) mode: u32,
    pub(crate) magic: u32,
    pub(crate) pad: [u32; 2],
}

impl Pref {
    pub(crate) fn new(w: u64, h: u64, fmt: u32, mode: u32) -> Pref {
        Pref {
            w: w as u16,
            h: h as u16,
            fmt,
            mode,
            magic: MAGIC,
            pad: [0; 2],
        }
    }

    pub(crate) fn read(addr: u64) -> Option<Pref> {
        let mut b = [0u8; 24];
        guest::read(addr, &mut b).ok()?;
        let u16_at = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
        let u32_at = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        Some(Pref {
            w: u16_at(0),
            h: u16_at(2),
            fmt: u32_at(4),
            mode: u32_at(8),
            magic: u32_at(12),
            pad: [0; 2],
        })
    }
}

// ---------- image pools and native images ----------

/// One image pool: the pictures that decoders have written into its buffers, and the memory of the game that its
/// native images are made over.
pub(crate) struct Pool {
    w: u16,
    h: u16,
    count: u32,
    imgs: Vec<Option<Arc<Frame>>>,
    mem: u64,
    size: u64,
    slot: usize,
    native: bool,
}

pub(crate) type PoolRef = Arc<Mutex<Pool>>;

impl Pool {
    /// The frame in buffer `i`.
    pub(crate) fn frame(&self, i: u32) -> Option<Arc<Frame>> {
        self.imgs.get(i as usize).cloned().flatten()
    }
}

fn native_stride(w: u32) -> u32 {
    (w * 4 + 63) & !63
}

fn native_size(w: u32, h: u32) -> u64 {
    (u64::from(native_stride(w)) * u64::from(h) + NATIVE_ALIGN - 1) & !(NATIVE_ALIGN - 1)
}

/// Writes picture `i` as RGBA into the memory of a native image, when the game asked for native images of the pool.
fn pool_native_write(p: &Pool, i: u32, frame: &Frame) {
    if !p.native || p.mem == 0 {
        return;
    }
    let (w, h) = (u32::from(p.w), u32::from(p.h));
    let each = native_size(w, h);
    if (u64::from(i) + 1) * each > p.size {
        return;
    }
    let dst = p.mem + u64::from(i) * each;
    let Ok(rgba) = frame.to_rgba(w, h) else {
        return;
    };
    let (row, stride) = (w as usize * 4, native_stride(w) as usize);
    if row == stride {
        let _ = guest::write(dst, &rgba);
    } else {
        for (y, line) in rgba.chunks_exact(row.max(1)).enumerate() {
            let _ = guest::write(dst + (y * stride) as u64, line);
        }
    }
    // The game does not write this memory, so the GPU would not see the new picture otherwise (a black movie).
    crate::guest_wrote(dst, each);
}

static POOLS: Mutex<BTreeMap<u32, PoolRef>> = Mutex::new(BTreeMap::new());
static NEXT_POOL: AtomicU32 = AtomicU32::new(1);
/// What a native image handle names: the pool of the handle's slot, `(slot << 8) | picture`.
static NATIVE_POOLS: Mutex<BTreeMap<usize, PoolRef>> = Mutex::new(BTreeMap::new());

fn handle_of(id: u32) -> u64 {
    (u64::from(MAGIC) << 32) | u64::from(id)
}

fn id_of(handle: u64) -> Option<u32> {
    ((handle >> 32) as u32 == MAGIC).then_some(handle as u32)
}

/// The pool whose handle the SDK object at `t` holds.
pub(crate) fn pool_of(t: u64) -> Option<PoolRef> {
    let id = id_of(guest::read_u64(t).ok()?)?;
    lock(&POOLS).get(&id).cloned()
}

/// `InitializeDecoderImagePool`: a pool of `n` pictures over `mem` and `size`.
pub(crate) fn pool_init(t: u64, mem: u64, size: u64, pref: &Pref, n: u32) -> u32 {
    let count = n.max(1);
    let pool = Pool {
        w: pref.w,
        h: pref.h,
        count,
        imgs: vec![None; count as usize],
        mem,
        size,
        slot: 0,
        native: false,
    };
    let id = NEXT_POOL.fetch_add(1, Ordering::Relaxed);
    lock(&POOLS).insert(id, Arc::new(Mutex::new(pool)));
    if guest::write_u64(t, handle_of(id)).is_err() {
        lock(&POOLS).remove(&id);
        return BAD;
    }
    0
}

/// `FinalizeDecoderImagePool`. A decoder that holds a buffer of the pool keeps the buffer without a pool to write its
/// pictures into.
pub(crate) fn pool_fin(t: u64) {
    let Some(id) = guest::read_u64(t).ok().and_then(id_of) else {
        return;
    };
    let Some(pool) = lock(&POOLS).remove(&id) else {
        return;
    };
    let decoders: Vec<Arc<Mutex<Dec>>> = lock(&DECODERS).values().cloned().collect();
    for d in decoders {
        for s in &mut lock(&d).slots {
            if s.pool.as_ref().is_some_and(|p| Arc::ptr_eq(p, &pool)) {
                s.pool = None;
            }
        }
    }
    let slot = lock(&pool).slot;
    if slot != 0 {
        lock(&NATIVE_POOLS).remove(&slot);
    }
    let _ = guest::write_u64(t, 0);
}

/// `GetDecoderImagePoolNativeImageHandle`: the handle that `nvnNativeImage` of the NVN front end knows for picture `i`.
/// From now on the pool keeps its pictures as RGBA in its own memory.
pub(crate) fn native_handle(pool: &PoolRef, i: u32) -> u32 {
    let mut p = lock(pool);
    if i >= p.count {
        return 0;
    }
    if p.slot == 0 {
        let mut native = lock(&NATIVE_POOLS);
        let Some(slot) = (1..=MAX_POOLS).find(|s| !native.contains_key(s)) else {
            red(&format!(
                "nn::codec: more than {MAX_POOLS} pools with native images"
            ));
            return 0;
        };
        native.insert(slot, Arc::clone(pool));
        p.slot = slot;
    }
    p.native = true;
    ((p.slot as u32) << 8) | i
}

/// The memory and the layout of a native image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeImage {
    /// The address of the first byte (a guest address for a decoder picture, a host address for a blank image).
    pub mem: u64,
    /// The bytes of the image.
    pub size: u64,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Bytes from one row to the next.
    pub stride: u32,
}

/// Memory on a 16 KB boundary that is zero at the start.
struct Block {
    ptr: *mut u8,
    layout: Layout,
}

// SAFETY: the block is plain bytes that are allocated once and freed in `Drop`. This crate never reads them. The host
// reads and writes them through the address it got.
unsafe impl Send for Block {}

impl Block {
    fn new(size: u64) -> Option<Block> {
        let layout =
            Layout::from_size_align(usize::try_from(size).ok()?, NATIVE_ALIGN as usize).ok()?;
        if layout.size() == 0 {
            return None;
        }
        // SAFETY: the layout has a size that is not zero.
        let ptr = unsafe { alloc_zeroed(layout) };
        (!ptr.is_null()).then_some(Block { ptr, layout })
    }
}

impl Drop for Block {
    fn drop(&mut self) {
        // SAFETY: `ptr` came from `alloc_zeroed` with this layout and is freed only here.
        unsafe { dealloc(self.ptr, self.layout) };
    }
}

struct Blank {
    w: u32,
    h: u32,
    size: u64,
    block: Block,
}

static BLANKS: Mutex<[Option<Blank>; MAX_BLANK]> = Mutex::new([const { None }; MAX_BLANK]);

fn blank_stride(w: u32) -> u32 {
    (w * 16 + 63) & !63
}

/// A blank native image: a picture that the system would fill and the host has no source for. All bytes are zero.
/// Its storage holds any uncompressed format up to 16 bytes a pixel. The handle has the top bit set. Gives 0 when
/// there is no room.
pub fn native_image_blank(w: u32, h: u32) -> u32 {
    let size = (u64::from(blank_stride(w)) * u64::from(h) + NATIVE_ALIGN - 1) & !(NATIVE_ALIGN - 1);
    let Some(block) = Block::new(size) else {
        return 0;
    };
    let mut blanks = lock(&BLANKS);
    let Some(k) = blanks.iter().position(Option::is_none) else {
        red(&format!(
            "native images: more than {MAX_BLANK} blank images"
        ));
        return 0;
    };
    blanks[k] = Some(Blank { w, h, size, block });
    0x8000_0000 | (k as u32 + 1)
}

/// Frees a blank native image. A texture on it must be finalized first, as on the console.
pub fn native_image_blank_free(handle: u32) {
    let k = (handle & 0x7fff_ffff).wrapping_sub(1) as usize;
    if handle >> 31 == 0 || k >= MAX_BLANK {
        return;
    }
    let blank = lock(&BLANKS)[k].take();
    drop(blank);
}

/// The picture that a native image handle names, for the NVN front end. `None` for a handle that no pool or blank
/// image holds.
pub fn native_image(handle: u32) -> Option<NativeImage> {
    let k = (handle & 0x7fff_ffff).wrapping_sub(1) as usize;
    if handle >> 31 != 0 && k < MAX_BLANK {
        if let Some(b) = &lock(&BLANKS)[k] {
            return Some(NativeImage {
                mem: b.block.ptr as u64,
                size: b.size,
                width: b.w,
                height: b.h,
                stride: blank_stride(b.w),
            });
        }
    }
    let (slot, i) = ((handle >> 8) as usize, handle & 0xff);
    let pool = lock(&NATIVE_POOLS).get(&slot).cloned()?;
    let p = lock(&pool);
    let (w, h) = (u32::from(p.w), u32::from(p.h));
    let each = native_size(w, h);
    (p.mem != 0 && i < p.count && (u64::from(i) + 1) * each <= p.size).then(|| NativeImage {
        mem: p.mem + u64::from(i) * each,
        size: each,
        width: w,
        height: h,
        stride: native_stride(w),
    })
}

/// The bytes that a pool of `n` pictures of `pref` needs for its native images.
pub(crate) fn pool_bytes(pref: &Pref, n: u32) -> u64 {
    native_size(u32::from(pref.w), u32::from(pref.h)) * u64::from(n.max(1))
}

/// The alignment of the memory of a pool.
pub(crate) const POOL_ALIGN: u64 = NATIVE_ALIGN;

// ---------- decoders ----------

#[derive(Clone, Default)]
struct Slot {
    pool: Option<PoolRef>,
    pidx: u32,
    reg: bool,
    filled: bool,
    generation: u32,
    filled_generation: u32,
    w: u16,
    h: u16,
    ts: i64,
    eos: bool,
}

struct Held {
    frame: Arc<Frame>,
    ts: i64,
    eos: bool,
}

/// What a call of the decoder asks the caller to do after it released the lock of the decoder.
#[derive(Default)]
pub(crate) struct Effects {
    /// `Some(true)` signals the submission ready event, `Some(false)` clears it.
    pub(crate) event: Option<bool>,
    /// The buffers that got a picture, with the generation that the picture belongs to.
    pub(crate) filled: Vec<(u32, u32)>,
}

type Stream = Option<(Box<dyn Backend>, u32, u32)>;

/// One decoder of the SDK.
pub(crate) struct Dec {
    id: u32,
    codec: Codec,
    event: u64,
    submits: u64,
    pictures: u64,
    stream: Stream,
    slots: Vec<Slot>,
    fifo: VecDeque<u32>,
    pend: VecDeque<u32>,
    held: VecDeque<Held>,
    eos_pending: bool,
    errors: u32,
    refused: u32,
}

/// Decodes one packet on a thread with a big stack, because the caller can be a guest thread with 8 KB.
fn decode_big(codec: Codec, stream: &mut Stream, data: &[u8]) -> Result<Option<Arc<Frame>>, String> {
    std::thread::scope(|s| {
        let job = std::thread::Builder::new().stack_size(DECODE_STACK).spawn_scoped(s, || match codec {
            Codec::Av1 => decode_av1(stream, crate::backend::open_av1, data),
            _ => decode_packet(stream, crate::backend::open, data),
        });
        match job {
            Ok(h) => h
                .join()
                .unwrap_or_else(|_| Err("the decoder thread panicked".to_string())),
            Err(e) => Err(format!("the decoder thread did not start: {e}")),
        }
    })
}

impl Dec {
    pub(crate) fn new(id: u32, codec: Codec, event: u64) -> Dec {
        Dec {
            id,
            codec,
            event,
            submits: 0,
            pictures: 0,
            stream: None,
            slots: vec![Slot::default(); BUFS],
            fifo: VecDeque::new(),
            pend: VecDeque::new(),
            held: VecDeque::new(),
            eos_pending: false,
            errors: 0,
            refused: 0,
        }
    }

    pub(crate) fn id(&self) -> u32 {
        self.id
    }

    pub(crate) fn event(&self) -> u64 {
        self.event
    }

    /// Registers buffer `i`, which is acquirable at once.
    pub(crate) fn register(&mut self, i: u32, pool: Option<PoolRef>, pidx: u32) -> u32 {
        trace(format_args!("dec {} register {i} pool idx {pidx} native {}", self.id, pool.as_ref().is_some_and(|p| lock(p).native)));
        let Some(s) = self.slots.get_mut(i as usize) else {
            red(&format!(
                "nn::codec: register of buffer {i}, a decoder has {BUFS}"
            ));
            return BAD;
        };
        *s = Slot {
            pool,
            pidx,
            reg: true,
            ..Slot::default()
        };
        if self.fifo.len() < FIFO {
            self.fifo.push_back(i);
        } else {
            red(&format!(
                "nn::codec: the queue of acquirable buffers is full at register {i}"
            ));
        }
        0
    }

    /// The buffer leaves the decoder: it is no longer acquirable, filled or waited for.
    pub(crate) fn unregister(&mut self, i: u32) -> u32 {
        let Some(s) = self.slots.get_mut(i as usize) else {
            return BAD;
        };
        s.reg = false;
        s.filled = false;
        s.pool = None;
        s.generation = s.generation.wrapping_add(1);
        self.fifo.retain(|&v| v != i);
        self.pend.retain(|&v| v != i);
        0
    }

    /// Stop and Flush: pictures not yet in a buffer and a pending end of stream are dropped, and the decoder starts
    /// over at the next key frame.
    pub(crate) fn reset(&mut self, fx: &mut Effects) {
        self.held.clear();
        self.eos_pending = false;
        self.stream = None;
        if self.event != 0 {
            fx.event = Some(true);
        }
    }

    /// A started decoder that has room takes packets, whatever became of the signal that Initialize gave. A player
    /// can clear that one and count only later signals.
    pub(crate) fn start(&self, fx: &mut Effects) {
        if self.event != 0 && self.held.len() < READY {
            fx.event = Some(true);
        }
    }

    fn hold(&mut self, frame: Arc<Frame>, ts: i64) -> bool {
        if self.held.len() >= HOLD {
            if self.errors < 4 {
                self.errors += 1;
                red(&format!(
                    "nn::codec {}: a packet gave more pictures than the host holds, one is dropped",
                    self.codec.name()
                ));
            }
            return false;
        }
        self.held.push_back(Held {
            frame,
            ts,
            eos: false,
        });
        true
    }

    /// One packet: decoded at once. Its pictures wait for released buffers. The end of stream marks the last picture,
    /// or comes as a buffer of its own. Gives the number of this submission.
    pub(crate) fn submit(
        &mut self,
        data: &[u8],
        ts: i64,
        eos: bool,
        fx: &mut Effects,
    ) -> Result<u64, u32> {
        trace(format_args!("dec {} submit {} bytes ts {ts} eos {eos} (held {}, pend {:?})", self.id, data.len(), self.held.len(), self.pend));
        if self.held.len() >= HOLD {
            if self.refused < 4 {
                self.refused += 1;
                red(&format!(
                    "nn::codec {}: a packet is refused with {} pictures waiting for a released buffer",
                    self.codec.name(),
                    self.held.len()
                ));
            }
            if self.event != 0 {
                fx.event = Some(false);
            }
            return Err(BUSY);
        }
        let mut got = false;
        if !data.is_empty() {
            match decode_big(self.codec, &mut self.stream, data) {
                Ok(Some(frame)) => got = self.hold(frame, ts),
                Ok(None) => {}
                Err(e) => {
                    if self.errors < 4 {
                        self.errors += 1;
                        red(&format!("nn::codec {}: {e}", self.codec.name()));
                    }
                }
            }
        }
        if eos {
            match self.held.back_mut() {
                Some(last) if got => last.eos = true,
                _ => self.eos_pending = true,
            }
        }
        self.submits += 1;
        let n = self.submits;
        self.drain(true, fx);
        Ok(n)
    }

    /// Puts pictures and the end of stream into the released buffers, in order. The event says that a packet can
    /// come: it rises when a packet was taken (`took`) or when room came back, not at every release. A player that
    /// counts each signal as one packet it may send would overrun [`HOLD`] if it rose at every release.
    fn drain(&mut self, took: bool, fx: &mut Effects) {
        let before = self.held.len();
        while !self.pend.is_empty() && (!self.held.is_empty() || self.eos_pending) {
            let Some(i) = self.pend.pop_front() else {
                break;
            };
            let picture = self.held.pop_front();
            let Some(s) = self.slots.get_mut(i as usize) else {
                continue;
            };
            match picture {
                Some(h) => {
                    s.w = h.frame.width() as u16;
                    s.h = h.frame.height() as u16;
                    s.ts = h.ts;
                    s.eos = h.eos;
                    if let Some(pool) = &s.pool {
                        let mut p = lock(pool);
                        if s.pidx < p.count {
                            pool_native_write(&p, s.pidx, &h.frame);
                            if let Some(img) = p.imgs.get_mut(s.pidx as usize) {
                                *img = Some(h.frame);
                            }
                        }
                    }
                    if self.pictures == 0 {
                        eprintln!(
                            "[codec] {} decoder {}: first picture {}x{} into buffer {i}",
                            self.codec.name(),
                            self.id,
                            s.w,
                            s.h
                        );
                    }
                    self.pictures += 1;
                }
                None => {
                    s.w = 0;
                    s.h = 0;
                    s.ts = 0;
                    s.eos = true;
                    self.eos_pending = false;
                }
            }
            if s.eos {
                eprintln!(
                    "[codec] {} decoder {}: end of stream after {} pictures",
                    self.codec.name(),
                    self.id,
                    self.pictures
                );
            }
            s.filled = true;
            s.filled_generation = s.generation;
            fx.filled.push((i, s.generation));
        }
        if self.event != 0 {
            if self.held.len() >= READY {
                fx.event = Some(false);
            } else if took || before >= READY {
                fx.event = Some(true);
            }
        }
    }

    /// The oldest acquirable buffer and its sync handle.
    pub(crate) fn acquire(&mut self) -> Result<(u32, SyncHandle), u32> {
        let i = self.fifo.pop_front().ok_or(NONE);
        trace(format_args!("dec {} acquire -> {i:?} (fifo {:?}, held {})", self.id, self.fifo, self.held.len()));
        let i = i?;
        let generation = self.slots.get(i as usize).map_or(0, |s| s.generation);
        Ok((
            i,
            SyncHandle {
                magic: SYNC_MAGIC,
                dec: self.id,
                idx: i,
                generation,
            },
        ))
    }

    /// The buffer waits for the next picture and can be acquired again.
    pub(crate) fn release(&mut self, i: u32, fx: &mut Effects) {
        trace(format_args!("dec {} release {i} (held {}, pend {:?})", self.id, self.held.len(), self.pend));
        match self.slots.get_mut(i as usize) {
            Some(s) if s.reg => {
                s.generation = s.generation.wrapping_add(1);
                s.filled = false;
            }
            _ => {
                red(&format!(
                    "nn::codec: release of buffer {i}, which is not registered"
                ));
                return;
            }
        }
        if self.pend.len() < BUFS {
            self.pend.push_back(i);
        }
        if self.fifo.len() < FIFO {
            self.fifo.push_back(i);
        } else {
            red(&format!(
                "nn::codec: the queue of acquirable buffers is full at release {i}"
            ));
        }
        self.drain(false, fx);
    }

    /// The width, height, time and end of stream flag of the picture in buffer `i`. [`NONE`] until it is filled.
    pub(crate) fn properties(&self, i: u32) -> Result<(u16, u16, i64, bool), u32> {
        let s = self.slots.get(i as usize).ok_or(BAD)?;
        trace(format_args!("dec {} properties {i} filled {}", self.id, s.filled));
        if s.filled {
            Ok((s.w, s.h, s.ts, s.eos))
        } else {
            Err(NONE)
        }
    }

    fn reached(&self, s: &SyncHandle) -> bool {
        self.slots
            .get(s.idx as usize)
            .is_some_and(|b| b.filled && b.filled_generation >= s.generation)
    }
}

static DECODERS: Mutex<BTreeMap<u32, Arc<Mutex<Dec>>>> = Mutex::new(BTreeMap::new());

/// The decoder whose handle the SDK object at `t` holds.
pub(crate) fn dec_of(t: u64) -> Option<Arc<Mutex<Dec>>> {
    let id = id_of(guest::read_u64(t).ok()?)?;
    lock(&DECODERS).get(&id).cloned()
}

/// The submission ready event of a decoder, a guest address.
pub(crate) fn dec_event(d: &Mutex<Dec>) -> u64 {
    lock(d).event()
}

/// Runs `f` on the decoder of the SDK object at `t` and then carries out what it asked for, with no lock held. `None`
/// when `t` holds no decoder.
pub(crate) fn dec_call<R>(t: u64, f: impl FnOnce(&mut Dec, &mut Effects) -> R) -> Option<R> {
    let dec = dec_of(t)?;
    let mut fx = Effects::default();
    let (r, id, event) = {
        let mut d = lock(&dec);
        let r = f(&mut d, &mut fx);
        (r, d.id(), d.event())
    };
    apply(id, event, fx);
    Some(r)
}

/// The picture in buffer `i` of the pool in the SDK object at `t`.
pub(crate) fn pool_frame(t: u64, i: u32) -> Option<Arc<Frame>> {
    let pool = pool_of(t)?;
    let p = lock(&pool);
    p.frame(i)
}

/// `Initialize…Decoder`: makes the decoder in the SDK object at `t`. VP9 and AV1 decode. The other codecs are refused
/// at once with a named line, so the game does not wait for pictures that never come.
pub(crate) fn create(t: u64, codec: Codec, w: u16, h: u16, input: u32, output: u32) -> u32 {
    let Some(cfg) = config() else {
        red("a decoder was made before the video service was initialised");
        return BAD;
    };
    if !matches!(codec, Codec::Vp9 | Codec::Av1) {
        red(&format!(
            "nn::codec {} decoders are not implemented, only VP9 and AV1. Initialize answers an error",
            codec.name()
        ));
        return BAD;
    }
    let event = t + EVENT_OFFSET;
    let dec = {
        let mut all = lock(&DECODERS);
        let Some(id) = (1..=MAX_DECODERS).find(|id| !all.contains_key(id)) else {
            red(&format!(
                "nn::codec: more than {MAX_DECODERS} video decoders"
            ));
            return BAD;
        };
        let dec = Arc::new(Mutex::new(Dec::new(id, codec, event)));
        all.insert(id, Arc::clone(&dec));
        dec
    };
    let id = lock(&dec).id();
    if guest::write_u64(t, handle_of(id)).is_err() {
        lock(&DECODERS).remove(&id);
        red(&format!(
            "the decoder object at {t:#x} is outside guest memory"
        ));
        return BAD;
    }
    cfg.host.bind_system_event(event);
    cfg.host.signal_system_event(event);
    eprintln!(
        "[codec] {} decoder {id}: {w}x{h}, input format {input}, output format {output} ({} on the host)",
        codec.name(),
        if codec == Codec::Av1 { "VideoToolbox or rav1d" } else { "VideoToolbox" }
    );
    0
}

/// `Finalize…Decoder`.
pub(crate) fn finalize(t: u64) -> u32 {
    let Some(id) = guest::read_u64(t).ok().and_then(id_of) else {
        return 0;
    };
    let Some(dec) = lock(&DECODERS).remove(&id) else {
        return 0;
    };
    let event = lock(&dec).event();
    // The event goes with the decoder, as in the SDK. Kept, it would stay bound, and the next decoder made in the same
    // memory would find its event bound and not get one.
    match crate::guest_api::os_fns() {
        Ok(f) => (f.destroy_system_event)(event),
        Err(e) => red(&format!(
            "the submission ready event of a decoder cannot be destroyed: {e}"
        )),
    }
    let _ = guest::write_u64(t, 0);
    0
}

/// Carries out what a call of a decoder asked for. The decoder lock is not held.
pub(crate) fn apply(dec: u32, event: u64, fx: Effects) {
    let Some(cfg) = config() else {
        return;
    };
    fire(&*cfg.host, dec, &fx.filled);
    match fx.event {
        Some(true) => cfg.host.signal_system_event(event),
        Some(false) => cfg.host.clear_system_event(event),
        None => {}
    }
}

/// Whether the picture that `s` waits for is in its buffer. A handle that the host did not make reads as signalled.
pub(crate) fn sync_reached(s: &SyncHandle) -> bool {
    let r = sync_reached_in(s);
    trace(format_args!("sync dec {} idx {} gen {} reached {r}", s.dec, s.idx, s.generation));
    r
}

fn sync_reached_in(s: &SyncHandle) -> bool {
    if s.magic != SYNC_MAGIC {
        static LOGGED: AtomicU32 = AtomicU32::new(0);
        if (s.magic != 0 || s.dec != 0) && LOGGED.fetch_add(1, Ordering::Relaxed) == 0 {
            red(&format!(
                "nn::codec: a sync handle that the host did not make ({:#x}) reads as signalled",
                s.magic
            ));
        }
        return true;
    }
    if s.generation == 0 {
        return true;
    }
    let dec = lock(&DECODERS).get(&s.dec).cloned();
    dec.is_none_or(|d| lock(&d).reached(s))
}

// ---------- sync point events ----------

struct Armed {
    event: u64,
    want: SyncHandle,
}

static ARMED: Mutex<Vec<Armed>> = Mutex::new(Vec::new());

/// Arms `event` for `want`. False when too many events are armed.
pub(crate) fn arm(event: u64, want: SyncHandle) -> bool {
    let mut a = lock(&ARMED);
    if a.len() >= MAX_ARMED {
        return false;
    }
    a.push(Armed { event, want });
    true
}

pub(crate) fn unarm(event: u64) {
    lock(&ARMED).retain(|a| a.event != event);
}

/// Signals the armed events that wait for a buffer that was filled.
fn fire(host: &dyn crate::Host, dec: u32, filled: &[(u32, u32)]) {
    if filled.is_empty() {
        return;
    }
    let mut due = Vec::new();
    lock(&ARMED).retain(|a| {
        let hit = a.want.dec == dec
            && filled
                .iter()
                .any(|&(i, g)| i == a.want.idx && g >= a.want.generation);
        if hit {
            due.push(a.event);
        }
        !hit
    });
    for e in due {
        host.signal_system_event(e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec() -> Dec {
        Dec::new(1, Codec::Vp9, 0)
    }

    #[test]
    fn nn_results_carry_the_description() {
        assert_eq!(BUSY, 0x6f | (3603 << 9));
        assert_eq!(NONE, 0x6f | (3604 << 9));
    }

    #[test]
    fn registered_buffers_are_acquired_in_order() {
        let mut d = dec();
        assert_eq!(d.register(2, None, 0), 0);
        assert_eq!(d.register(5, None, 1), 0);
        assert_eq!(d.acquire().map(|(i, _)| i), Ok(2));
        assert_eq!(d.acquire().map(|(i, _)| i), Ok(5));
        assert_eq!(d.acquire().map(|(i, _)| i), Err(NONE));
    }

    #[test]
    fn a_buffer_that_does_not_exist_is_refused() {
        let mut d = dec();
        assert_eq!(d.register(BUFS as u32, None, 0), BAD);
        assert_eq!(d.unregister(BUFS as u32), BAD);
    }

    #[test]
    fn the_acquire_queue_is_eight_deep() {
        let mut d = dec();
        for i in 0..10 {
            d.register(i, None, 0);
        }
        assert_eq!(d.fifo.len(), FIFO);
    }

    #[test]
    fn a_picture_goes_into_the_first_released_buffer() {
        let mut d = dec();
        d.register(0, None, 0);
        let (i, sync) = d.acquire().unwrap_or_else(|_| unreachable!());
        let mut fx = Effects::default();
        d.release(i, &mut fx);
        assert!(!d.reached(&sync) || sync.generation == 0);
        d.held.push_back(Held {
            frame: Arc::new(grey()),
            ts: 7,
            eos: false,
        });
        d.drain(true, &mut fx);
        assert_eq!(d.properties(0), Ok((2, 2, 7, false)));
        assert_eq!(fx.filled.len(), 1);
    }

    #[test]
    fn a_handle_of_the_old_release_is_not_reached_before_the_picture() {
        let mut d = dec();
        d.register(0, None, 0);
        let mut fx = Effects::default();
        d.release(0, &mut fx);
        let (_, sync) = d.acquire().unwrap_or_else(|_| unreachable!());
        assert!(!d.reached(&sync));
        d.held.push_back(Held {
            frame: Arc::new(grey()),
            ts: 0,
            eos: false,
        });
        d.drain(true, &mut fx);
        assert!(d.reached(&sync));
    }

    #[test]
    fn the_end_of_stream_gets_a_buffer_of_its_own() {
        let mut d = dec();
        d.register(3, None, 0);
        let mut fx = Effects::default();
        d.release(3, &mut fx);
        d.eos_pending = true;
        d.drain(true, &mut fx);
        assert_eq!(d.properties(3), Ok((0, 0, 0, true)));
        assert!(!d.eos_pending);
    }

    #[test]
    fn a_full_queue_refuses_packets_and_clears_the_event() {
        let mut d = Dec::new(1, Codec::Vp9, 0x1000);
        for _ in 0..HOLD {
            d.held.push_back(Held {
                frame: Arc::new(grey()),
                ts: 0,
                eos: false,
            });
        }
        let mut fx = Effects::default();
        assert_eq!(d.submit(&[1], 0, false, &mut fx), Err(BUSY));
        assert_eq!(fx.event, Some(false));
    }

    #[test]
    fn the_event_goes_down_at_ready_pictures_and_up_when_room_comes_back() {
        let mut d = Dec::new(1, Codec::Vp9, 0x1000);
        for _ in 0..READY {
            d.held.push_back(Held {
                frame: Arc::new(grey()),
                ts: 0,
                eos: false,
            });
        }
        let mut fx = Effects::default();
        d.drain(false, &mut fx);
        assert_eq!(fx.event, Some(false));
        d.register(0, None, 0);
        d.release(0, &mut fx);
        assert_eq!(d.held.len(), READY - 1);
        assert_eq!(fx.event, Some(true));
    }

    #[test]
    fn unregister_drops_the_buffer_from_both_queues() {
        let mut d = dec();
        d.register(1, None, 0);
        let mut fx = Effects::default();
        d.release(1, &mut fx);
        assert_eq!(d.unregister(1), 0);
        assert!(d.fifo.is_empty() && d.pend.is_empty());
    }

    #[test]
    fn a_foreign_sync_handle_reads_as_signalled() {
        assert!(sync_reached(&SyncHandle::default()));
        assert!(sync_reached(&SyncHandle {
            magic: SYNC_MAGIC,
            dec: 1,
            idx: 0,
            generation: 0
        }));
    }

    #[test]
    fn native_sizes_follow_the_page_and_the_row_alignment() {
        assert_eq!(native_stride(1920), 7680);
        assert_eq!(native_stride(100), 448);
        assert_eq!(native_size(1920, 1080) % NATIVE_ALIGN, 0);
        assert!(native_size(1920, 1080) >= 7680 * 1080);
    }

    #[test]
    fn a_blank_image_has_its_memory_until_it_is_freed() {
        let h = native_image_blank(16, 16);
        assert_ne!(h, 0);
        let img = native_image(h);
        assert!(img.is_some_and(|i| i.width == 16
            && i.stride == blank_stride(16)
            && i.mem % NATIVE_ALIGN == 0));
        native_image_blank_free(h);
        assert_eq!(native_image(h), None);
    }

    fn grey() -> Frame {
        Frame::from_nv12(
            2,
            2,
            3,
            crate::frame::Nv12 {
                y: vec![16; 4],
                y_stride: 2,
                uv: vec![128; 2],
                uv_stride: 2,
            },
        )
    }
}
