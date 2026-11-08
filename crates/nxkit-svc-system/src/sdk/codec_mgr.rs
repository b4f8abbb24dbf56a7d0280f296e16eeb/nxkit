//! The SDK 21 hardware Opus decoder manager. Source: nxnative `dm/host/hle_codec.c`, checked on 2026-11-08.
//!
//! The asynchronous interface over `hwopus` has a manager that owns decoders and a pool of I/O buffers. The game
//! enqueues one packet per call, then calls `ExecuteHardwareOpusDecoderDecoding` on each manager once per audio frame.
//! Then it dequeues each finished frame (960 samples a channel at 48 kHz) and reads the request counts.
//!
//! `Execute` decodes every waiting packet with the software `OpusDecoder` of the game on the calling thread and
//! signals the event of the manager. So a request waits until `Execute`, never executes, and is completed until it is
//! dequeued. Each request holds one I/O buffer from enqueue to dequeue.
//!
//! The first 8 bytes of the manager object hold the host id of the manager, because the game tells an initialized
//! manager by its first word. The first 8 bytes of a decoder object hold the address of its software state, as in the
//! older C interface of `codec.rs`. The queues of a decoder live on the host. A packet and its PCM pass through one
//! scratch block of guest memory, which is safe because `Execute` holds the state lock for the whole decode.

use super::codec::{give_block, soft_decode, soft_fini, soft_init, soft_reset, soft_work_size, take_block};
use super::{fatal, get_bytes, get_i32, get_u64, log, put_bytes, put_i32, put_u64, put_zero};
use crate::event::{self, Block, guest_alloc};
use crate::sys::{call_guest, call_guest7};
use crate::util::lock;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// The result of the hwopus calls that fail: the packet is refused, the manager is gone, or nothing is completed.
const RES_OPUS: u32 = 0x3a6f;
/// The most bytes of a packet with its header. A 20 ms frame of at most 6 streams is far below it.
const MAX_PACKET: usize = 0x1800;
/// The most bytes of one decoded frame: 60 ms, stereo, 16-bit.
const MAX_PCM: usize = 960 * 2 * 2 * 3;
/// The samples of the 20 ms frame that stands for a packet that the decoder refuses.
const SILENCE_SAMPLES: i32 = 960;
/// The offsets in the scratch block: the packet, the PCM, then the `consumed` and `samples` outputs.
const PKT_AT: u64 = 0;
const PCM_AT: u64 = 0x2000;
const OUT_AT: u64 = 0x5000;
const SCRATCH: u64 = 0x6000;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static SCRATCH_BLOCK: Block = Block::new();
static REFUSED: AtomicU32 = AtomicU32::new(0);

/// A decoded frame: its PCM bytes and its samples a channel.
struct Done {
    pcm: Vec<u8>,
    samples: i32,
}

struct Dec {
    /// The id of the manager, or 0 once the game finalized it.
    mgr: u64,
    soft: u64,
    work: u64,
    work_size: u64,
    channels: u32,
    wait: VecDeque<Vec<u8>>,
    done: VecDeque<Done>,
}

struct Mgr {
    event: u64,
    io_total: i32,
    io_used: i32,
    /// The decoders in the order of their initialization, by the address of the game object.
    decs: Vec<u64>,
}

struct State {
    mgrs: BTreeMap<u64, Mgr>,
    decs: BTreeMap<u64, Dec>,
    /// Work buffers of finalized decoders, by size and address.
    work: Vec<(u64, u64)>,
}

static STATE: Mutex<State> = Mutex::new(State { mgrs: BTreeMap::new(), decs: BTreeMap::new(), work: Vec::new() });

/// The id of the manager at `gm`. A manager that the host did not initialize stops the game.
fn mgr_id(gm: u64) -> u64 {
    let id = get_u64(gm);
    if id == 0 {
        fatal(&format!("Opus decoder manager {gm:#x} was not initialized by the host"));
    }
    id
}

fn mgr_of<'a>(s: &'a mut State, gm: u64) -> (u64, &'a mut Mgr) {
    let id = mgr_id(gm);
    match s.mgrs.get_mut(&id) {
        Some(m) => (id, m),
        None => fatal(&format!("Opus decoder manager {gm:#x} was not initialized by the host")),
    }
}

fn dec_of<'a>(s: &'a mut State, gd: u64) -> &'a mut Dec {
    match s.decs.get_mut(&gd) {
        Some(d) => d,
        None => fatal(&format!("Opus decoder {gd:#x} was not initialized by the host")),
    }
}

/// A work buffer of at least `size` bytes, zeroed: a finalized decoder's one, else a new one.
fn take_work(s: &mut State, size: u64) -> u64 {
    if let Some(i) = s.work.iter().position(|(n, _)| *n >= size) {
        let (n, a) = s.work.swap_remove(i);
        put_zero(a, n);
        return a;
    }
    let a = guest_alloc(size);
    if a == 0 {
        fatal("no memory for an Opus decoder work buffer");
    }
    a
}

/// Drops every request of the decoder and gives its I/O buffers back to the manager.
fn clear(mgrs: &mut BTreeMap<u64, Mgr>, d: &mut Dec) {
    if let Some(m) = mgrs.get_mut(&d.mgr) {
        m.io_used -= (d.wait.len() + d.done.len()) as i32;
    }
    d.wait.clear();
    d.done.clear();
}

/// `GetHardwareOpusDecoderManagerWorkBufferSize`: the host keeps the state, the work buffer is not used.
extern "C" fn work_size(_rate: u32, _channels: u32, _frame: u64, _decoders: u32, _io: u32) -> u64 {
    0x1000
}

/// `InitializeHardwareOpusDecoderManager(manager*, SystemEventType*, rate, channels, TimeSpan frame, decoders, io,
/// work, size)`. The ninth argument is on the stack and unused. The SDK creates the event and the game destroys it after
/// the finalize.
extern "C" fn mgr_initialize(gm: u64, ev: u64, _rate: u32, _channels: u32, _frame: u64, _decoders: u32, io: u32, _work: u64) -> u32 {
    event::bind(ev, true);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    lock(&STATE).mgrs.insert(id, Mgr { event: ev, io_total: io as i32, io_used: 0, decs: Vec::new() });
    put_u64(gm, id);
    0
}

/// `FinalizeHardwareOpusDecoderManager`: the decoders stay, without a manager.
extern "C" fn mgr_finalize(gm: u64) {
    let id = get_u64(gm);
    if id == 0 {
        return;
    }
    let mut s = lock(&STATE);
    if let Some(m) = s.mgrs.remove(&id) {
        for gd in m.decs {
            if let Some(d) = s.decs.get_mut(&gd) {
                d.mgr = 0;
            }
        }
    }
    put_u64(gm, 0);
}

extern "C" fn free_io(gm: u64) -> i32 {
    let mut s = lock(&STATE);
    let (_, m) = mgr_of(&mut s, gm);
    m.io_total - m.io_used
}

/// `InitializeHardwareOpusDecoder(decoder*, rate, channels, TimeSpan frame, manager*)`: the software `OpusDecoder`
/// replaces the hardware one.
extern "C" fn dec_initialize(gd: u64, rate: u32, channels: u32, _frame: u64, gm: u64) -> u32 {
    let mut s = lock(&STATE);
    let (id, _) = mgr_of(&mut s, gm);
    if s.decs.contains_key(&gd) {
        drop_decoder(&mut s, gd);
    }
    let ws = call_guest(soft_work_size(), [u64::from(rate), u64::from(channels), 0, 0, 0, 0]);
    let soft = take_block();
    let work = take_work(&mut s, ws);
    let r = call_guest(soft_init(), [soft, u64::from(rate), u64::from(channels), work, ws, 0]) as u32;
    if r != 0 {
        give_block(soft);
        s.work.push((ws, work));
        return r;
    }
    put_u64(gd, soft);
    s.decs.insert(gd, Dec { mgr: id, soft, work, work_size: ws, channels, wait: VecDeque::new(), done: VecDeque::new() });
    if let Some(m) = s.mgrs.get_mut(&id) {
        m.decs.push(gd);
    }
    0
}

/// Removes the manager decoder at `gd`, finalizes its software state and returns its buffers. Gives false when `gd` is
/// not a manager decoder.
fn drop_decoder(s: &mut State, gd: u64) -> bool {
    let Some(mut d) = s.decs.remove(&gd) else { return false };
    clear(&mut s.mgrs, &mut d);
    if let Some(m) = s.mgrs.get_mut(&d.mgr) {
        m.decs.retain(|x| *x != gd);
    }
    call_guest(soft_fini(), [d.soft, 0, 0, 0, 0, 0]);
    give_block(d.soft);
    s.work.push((d.work_size, d.work));
    put_u64(gd, 0);
    true
}

/// `FinalizeHardwareOpusDecoder` of a manager decoder. Gives false for a decoder of the older interface.
pub(super) fn finalize_decoder(gd: u64) -> bool {
    drop_decoder(&mut lock(&STATE), gd)
}

/// `ResetHardwareOpusDecoder` of a manager decoder: every request is dropped. Gives false for a decoder of the older
/// interface.
pub(super) fn reset_decoder(gd: u64) -> bool {
    let mut s = lock(&STATE);
    let State { mgrs, decs, .. } = &mut *s;
    let Some(d) = decs.get_mut(&gd) else { return false };
    clear(mgrs, d);
    call_guest(soft_reset(), [d.soft, 0, 0, 0, 0, 0]);
    true
}

/// `EnqueueOpusDataToHardwareOpusDecoder(decoder*, size_t* consumed, const void* in, size_t in_size)`.
extern "C" fn enqueue(gd: u64, consumed: u64, input: u64, in_size: u64) -> u32 {
    put_u64(consumed, 0);
    let mut s = lock(&STATE);
    let d = dec_of(&mut s, gd);
    let mid = d.mgr;
    if mid == 0 {
        return RES_OPUS;
    }
    let n = in_size as usize;
    if in_size < 8 || n > MAX_PACKET {
        log(&format!("[RED] codec: Opus packet of {in_size} bytes enqueued"));
        return RES_OPUS;
    }
    let packet = get_bytes(input, n);
    // The game checks the free count first, so a full pool is an error of the game.
    let Some(m) = s.mgrs.get_mut(&mid) else { return RES_OPUS };
    if m.io_used >= m.io_total {
        return RES_OPUS;
    }
    m.io_used += 1;
    dec_of(&mut s, gd).wait.push_back(packet);
    put_u64(consumed, in_size);
    0
}

/// `ExecuteHardwareOpusDecoderDecoding(manager*)`: the software decoder decodes the waiting packets here. A frame that
/// the decoder refuses completes as 20 ms of silence, so the stream of the game stays in step.
extern "C" fn execute(gm: u64) -> u32 {
    let scratch = SCRATCH_BLOCK.addr(SCRATCH);
    if scratch == 0 {
        fatal("no memory for the Opus scratch block");
    }
    let (ev, any) = {
        let mut s = lock(&STATE);
        let (_, m) = mgr_of(&mut s, gm);
        let (ev, order) = (m.event, m.decs.clone());
        let mut any = false;
        for gd in order {
            let d = dec_of(&mut s, gd);
            while let Some(pkt) = d.wait.pop_front() {
                let done = decode(d, scratch, &pkt);
                d.done.push_back(done);
                any = true;
            }
        }
        (ev, any)
    };
    if any {
        event::signal_bound(ev);
    }
    0
}

/// Decodes one packet through the scratch block.
fn decode(d: &Dec, scratch: u64, pkt: &[u8]) -> Done {
    put_bytes(scratch + PKT_AT, pkt);
    put_zero(scratch + OUT_AT, 16);
    let (used, samples) = (scratch + OUT_AT, scratch + OUT_AT + 8);
    let a = [d.soft, used, samples, scratch + PCM_AT, MAX_PCM as u64, scratch + PKT_AT];
    let res = call_guest7(soft_decode(), a, pkt.len() as u64) as u32;
    let got = get_i32(samples);
    let bytes = got.max(0) as usize * d.channels as usize * 2;
    if res == 0 && got > 0 && bytes <= MAX_PCM {
        return Done { pcm: get_bytes(scratch + PCM_AT, bytes), samples: got };
    }
    if REFUSED.fetch_add(1, Ordering::Relaxed) < 4 {
        log(&format!("[RED] codec: software Opus decode of a {}-byte packet failed ({res:#x}), silence instead", pkt.len()));
    }
    Done { pcm: vec![0; SILENCE_SAMPLES as usize * d.channels as usize * 2], samples: SILENCE_SAMPLES }
}

/// `DequeuePcmInterleavedDataFromHardwareOpusDecoder(decoder*, int* samples, short* out, size_t out_size)`.
extern "C" fn dequeue(gd: u64, samples: u64, out: u64, out_size: u64) -> u32 {
    put_i32(samples, 0);
    let mut s = lock(&STATE);
    let State { mgrs, decs, .. } = &mut *s;
    let Some(d) = decs.get_mut(&gd) else {
        fatal(&format!("Opus decoder {gd:#x} was not initialized by the host"));
    };
    if d.mgr == 0 {
        return RES_OPUS;
    }
    let Some(front) = d.done.front() else { return RES_OPUS };
    if front.pcm.len() as u64 > out_size {
        log(&format!("[RED] codec: a {}-byte Opus frame for a {out_size}-byte buffer", front.pcm.len()));
        return RES_OPUS;
    }
    let Some(r) = d.done.pop_front() else { return RES_OPUS };
    if let Some(m) = mgrs.get_mut(&d.mgr) {
        m.io_used -= 1;
    }
    put_bytes(out, &r.pcm);
    put_i32(samples, r.samples);
    0
}

fn count(gd: u64, f: impl FnOnce(&Dec) -> usize) -> i32 {
    let mut s = lock(&STATE);
    let d = dec_of(&mut s, gd);
    if d.mgr == 0 { 0 } else { f(d) as i32 }
}

/// Packets that are not decoded yet.
extern "C" fn waiting(gd: u64) -> i32 {
    count(gd, |d| d.wait.len())
}

/// `Execute` decodes synchronously, so none runs in the background.
extern "C" fn executing(gd: u64) -> i32 {
    count(gd, |_| 0)
}

/// Decoded frames that are not dequeued yet.
extern "C" fn completed(gd: u64) -> i32 {
    count(gd, |d| d.done.len())
}

table! {
    "_ZN2nn5codec43GetHardwareOpusDecoderManagerWorkBufferSizeEiiNS_8TimeSpanEii" => work_size,
    "_ZN2nn5codec36InitializeHardwareOpusDecoderManagerEPNS0_30HardwareOpusDecoderManagerTypeEPNS_2os15SystemEventTypeEiiNS_8TimeSpanEiiPvm" => mgr_initialize,
    "_ZN2nn5codec34FinalizeHardwareOpusDecoderManagerEPNS0_30HardwareOpusDecoderManagerTypeE" => mgr_finalize,
    "_ZN2nn5codec46GetHardwareOpusDecoderManagerFreeIoBufferCountEPKNS0_30HardwareOpusDecoderManagerTypeE" => free_io,
    "_ZN2nn5codec29InitializeHardwareOpusDecoderEPNS0_23HardwareOpusDecoderTypeEiiNS_8TimeSpanEPNS0_30HardwareOpusDecoderManagerTypeE" => dec_initialize,
    "_ZN2nn5codec36EnqueueOpusDataToHardwareOpusDecoderEPNS0_23HardwareOpusDecoderTypeEPmPKvm" => enqueue,
    "_ZN2nn5codec34ExecuteHardwareOpusDecoderDecodingEPNS0_30HardwareOpusDecoderManagerTypeE" => execute,
    "_ZN2nn5codec48DequeuePcmInterleavedDataFromHardwareOpusDecoderEPNS0_23HardwareOpusDecoderTypeEPiPsm" => dequeue,
    "_ZN2nn5codec47GetHardwareOpusDecoderWaitingDecodeRequestCountEPKNS0_23HardwareOpusDecoderTypeE" => waiting,
    "_ZN2nn5codec49GetHardwareOpusDecoderExecutingDecodeRequestCountEPKNS0_23HardwareOpusDecoderTypeE" => executing,
    "_ZN2nn5codec49GetHardwareOpusDecoderCompletedDecodeRequestCountEPKNS0_23HardwareOpusDecoderTypeE" => completed,
}
