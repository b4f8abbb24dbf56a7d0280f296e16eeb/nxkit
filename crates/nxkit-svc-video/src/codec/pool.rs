//! The pools of output pictures: the sync handles, the native images and the blank images the game can read.

use super::*;

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
    pub(super) w: u16,
    pub(super) h: u16,
    pub(super) count: u32,
    pub(super) imgs: Vec<Option<Arc<Frame>>>,
    pub(super) mem: u64,
    pub(super) size: u64,
    pub(super) slot: usize,
    pub(super) native: bool,
}

pub(crate) type PoolRef = Arc<Mutex<Pool>>;

impl Pool {
    /// The frame in buffer `i`.
    pub(crate) fn frame(&self, i: u32) -> Option<Arc<Frame>> {
        self.imgs.get(i as usize).cloned().flatten()
    }
}

pub(super) fn native_stride(w: u32) -> u32 {
    (w * 4 + 63) & !63
}

pub(super) fn native_size(w: u32, h: u32) -> u64 {
    (u64::from(native_stride(w)) * u64::from(h) + NATIVE_ALIGN - 1) & !(NATIVE_ALIGN - 1)
}

/// Writes picture `i` as RGBA into the memory of a native image, when the game asked for native images of the pool.
pub(super) fn pool_native_write(p: &Pool, i: u32, frame: &Frame) {
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

pub(super) static POOLS: Mutex<BTreeMap<u32, PoolRef>> = Mutex::new(BTreeMap::new());
pub(super) static NEXT_POOL: AtomicU32 = AtomicU32::new(1);
/// What a native image handle names: the pool of the handle's slot, `(slot << 8) | picture`.
pub(super) static NATIVE_POOLS: Mutex<BTreeMap<usize, PoolRef>> = Mutex::new(BTreeMap::new());

pub(super) fn handle_of(id: u32) -> u64 {
    (u64::from(MAGIC) << 32) | u64::from(id)
}

pub(super) fn id_of(handle: u64) -> Option<u32> {
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
pub(super) struct Block {
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

pub(super) struct Blank {
    w: u32,
    h: u32,
    size: u64,
    block: Block,
}

pub(super) static BLANKS: Mutex<[Option<Blank>; MAX_BLANK]> =
    Mutex::new([const { None }; MAX_BLANK]);

pub(super) fn blank_stride(w: u32) -> u32 {
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
    if handle >> 31 != 0
        && k < MAX_BLANK
        && let Some(b) = &lock(&BLANKS)[k]
    {
        return Some(NativeImage {
            mem: b.block.ptr as u64,
            size: b.size,
            width: b.w,
            height: b.h,
            stride: blank_stride(b.w),
        });
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
