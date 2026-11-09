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
//!
//! # Threading
//!
//! A decoder is one `Dec` behind one mutex. The guest threads call it (submit, release, acquire, sync waits, reset),
//! and so does, for a stream that decodes on the CPU, the worker thread of the stream.
//!
//! - VP9 and the AV1 of VideoToolbox decode in the calling thread of `submit`, on a scoped thread with a big stack.
//! - AV1 on the CPU (rav1d) has a worker thread for each stream. The first packet that the backend answers with a
//!   [`Pipeline`] switches the decoder to it. From then on `submit` only copies the packet and its time into the bounded
//!   input queue (`Dec::queued`) and returns. The worker owns the rav1d context and sends the packets. rav1d works on
//!   two frames at once (`Dav1d::streaming_delay`), so a unit's picture comes out of a later send. The worker never
//!   drains rav1d before the end of the stream (`Pipeline::waits`): the game keeps the queue filled while the event
//!   is up, and `Dec::lag` keeps the event up for the frames inside. The game's threads never wait for the decode.
//! - The worker puts pictures into `Dec::held` under the lock and runs the same `drain` as the guest, which moves them
//!   into released buffers and decides the event. The effects (`Effects`) are carried out after the lock is released,
//!   on the worker thread: the host signals a system event of the kernel from any thread (the event is bound when the
//!   decoder is made, on a guest thread). `Dec::order` is held from the decision to the effect, so two threads cannot
//!   apply a clear and a signal in the wrong order.
//! - [`HOLD`] counts packets queued, frames in flight and pictures held (`Dec::load`). The event goes down when the
//!   load reaches `Dec::ready_limit` and up again when it falls below. Only the guest (release, submit, reset) and the
//!   worker (a packet that gave no picture) lower the load, and each of them runs the event logic when it does.
//! - Stop and Flush clear the queue, ask the worker to flush rav1d and wait until it is idle. Dropping the stream
//!   (`Finalize`) stops the worker and joins it.

use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use nxkit_abi::guest;

use crate::config;
use crate::decoder::{Backend, Pic, Pipeline, decode_packet};
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
/// Pictures that the worker of a CPU stream decodes ahead of the released buffers. Then it waits for the game, as a
/// hardware decoder stops when its output buffers are full. The input queue keeps taking packets meanwhile.
pub(crate) const LOOKAHEAD: usize = 3;
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
/// The stack of the worker thread of a stream.
const WORKER_STACK: usize = 8 << 20;

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
fn diag_codec() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("NXKIT_DIAG").is_ok_and(|v| {
            v.split(',')
                .any(|c| c == "codec" || c == "codecall" || c == "all")
        })
    })
}

pub(crate) fn trace(what: std::fmt::Arguments<'_>) {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU32, Ordering};
    static ON: OnceLock<(bool, u32)> = OnceLock::new();
    static N: AtomicU32 = AtomicU32::new(0);
    // `codec`: the first 600 calls. `codecall`: every call, with the time in ms, to see where a stream sticks late.
    let (on, cap) = *ON.get_or_init(|| {
        let v = std::env::var("NXKIT_DIAG").unwrap_or_default();
        let all = v.split(',').any(|c| c == "codecall");
        (
            all || v.split(',').any(|c| c == "codec" || c == "all"),
            if all { u32::MAX } else { 600 },
        )
    });
    if on && N.fetch_add(1, Ordering::Relaxed) < cap {
        static T0: OnceLock<std::time::Instant> = OnceLock::new();
        let ms = T0
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_millis();
        eprintln!("DIAG codec {ms} {what}");
    }
}

fn warn(what: &str) {
    eprintln!("nxkit-video: warning: {what}");
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

mod dec;
mod pool;
mod registry;
#[cfg(test)]
mod tests;
mod worker;

use dec::*;
pub(crate) use pool::*;
pub use pool::{NativeImage, native_image, native_image_blank, native_image_blank_free};
pub(crate) use registry::*;
use worker::*;

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

/// Opens the AV1 backend of a stream.
type OpenAv1 = fn() -> Result<Box<dyn Backend>, String>;

/// Carries out the effects of a call: `(decoder id, event, effects)`.
type Applier = Arc<dyn Fn(u32, u64, Effects) + Send + Sync>;

/// A packet for the worker.
struct Job {
    data: Vec<u8>,
    ts: i64,
    eos: bool,
}

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
    open_av1: OpenAv1,
    applier: Applier,
    me: Weak<Mutex<Dec>>,
    // The worker of a stream that decodes on the CPU. All the fields below are idle for the other streams.
    /// The worker, once the stream switched to it.
    worker: Option<JoinHandle<()>>,
    /// Wakes the worker (a packet, a flush, a stop) and the threads that wait for it (a flush).
    cv: Arc<Condvar>,
    /// Held from the decision of an effect to its execution.
    order: Arc<Mutex<()>>,
    /// Packets that wait for the worker.
    queued: VecDeque<Job>,
    /// Packets that the worker took and whose picture is not in `held` yet.
    inflight: usize,
    /// Frames the decoder works on at once, the room that the queue of packets has on top of [`READY`].
    lag: usize,
    /// The worker is to flush the decoder. Its pictures are dropped until it has.
    flush_req: bool,
    /// What the last effect did to the submission ready event: `Some(true)` signalled, `Some(false)` cleared.
    event_state: Option<bool>,
    /// The packets refused with BUSY, and the last state line (`NXKIT_DIAG=codec`).
    refusals: u64,
    last_diag: Instant,
    /// The worker is in a call of the decoder, with the lock released.
    busy: bool,
    stop: bool,
}

/// What a packet did in the call that submitted it.
enum Step {
    Picture(Option<Arc<Frame>>),
    /// The backend runs the stream on a worker from now on.
    Pipeline(Box<dyn Pipeline>),
}

/// Decodes one packet on a thread with a big stack, because the caller can be a guest thread with 8 KB.
fn decode_big(
    codec: Codec,
    stream: &mut Stream,
    open_av1: OpenAv1,
    data: &[u8],
) -> Result<Step, String> {
    std::thread::scope(|s| {
        let job = std::thread::Builder::new()
            .stack_size(DECODE_STACK)
            .spawn_scoped(s, || match codec {
                Codec::Av1 => decode_av1_step(stream, open_av1, data),
                _ => decode_packet(stream, crate::backend::open, data).map(Step::Picture),
            });
        match job {
            Ok(h) => h
                .join()
                .unwrap_or_else(|_| Err("the decoder thread panicked".to_string())),
            Err(e) => Err(format!("the decoder thread did not start: {e}")),
        }
    })
}

/// One packet of an AV1 stream: the backend may take the stream over for a worker, else it decodes the packet.
fn decode_av1_step(stream: &mut Stream, open: OpenAv1, data: &[u8]) -> Result<Step, String> {
    if data.is_empty() {
        return Ok(Step::Picture(None));
    }
    if stream.is_none() {
        *stream = Some((open()?, 0, 0));
    }
    let Some((b, _, _)) = stream else {
        return Ok(Step::Picture(None));
    };
    if let Some(p) = b.pipeline(data) {
        *stream = None;
        return Ok(Step::Pipeline(p));
    }
    b.decode(data).map(Step::Picture)
}
