//! The `movie::Decoder` of the game's SDK, for a video stream.
//!
//! The SDK class is a MediaCodec-like client protocol: the game takes an input buffer, fills it with one packet, sends
//! it, and later takes the decoded picture from an output slot. The class here is the host side of that protocol. It
//! knows nothing about guest objects. `exports.rs` maps the methods of the SDK to it.
//!
//! The decoding runs on a thread of its own. Events for the game (`SignalEvent`) are guest code, so they are handed to
//! the host to run on a guest thread through [`Io`].

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::frame::Frame;
use crate::vp9::{KeyFrame, parse_key_frame};

/// The input buffers a decoder hands out.
pub(crate) const IN_COUNT: usize = 4;
/// The size of one input buffer.
pub(crate) const IN_CAP: usize = 4 << 20;
/// The output slots.
pub(crate) const OUT_COUNT: usize = 16;
/// Packets that wait for an output slot.
const MAX_PENDING: usize = 4;
/// How long an input that the game held across a flush is not handed out again.
const STALE: Duration = Duration::from_millis(250);
/// The delay of the "input available" event after `Start`.
const START_DELAY: Duration = Duration::from_millis(2);

/// No implementation behind the object.
pub(crate) const ERR_NO_IMPL: u32 = 0x1000;
/// No input buffer is free.
pub(crate) const ERR_NO_INPUT: u32 = 0x1011;
/// No output is ready.
pub(crate) const ERR_NO_OUTPUT: u32 = 0x100e;
/// An argument is out of range.
pub(crate) const ERR_BAD_ARG: u32 = 0x100b;
/// `Start` on an object without implementation.
pub(crate) const ERR_START: u32 = 0x2002;
/// `Stop` on an object without implementation.
pub(crate) const ERR_STOP: u32 = 0x2003;

/// The end-of-stream flag of a packet.
pub(crate) const FLAG_EOS: u32 = 4;

const DEFAULT_WIDTH: u32 = 1920;
const DEFAULT_HEIGHT: u32 = 1080;
/// `nv12-colorspace` before a stream says otherwise: BT.709 limited range.
const DEFAULT_COLORSPACE: u32 = 3;

/// A video decoder of one stream type. It turns a packet into a picture.
pub(crate) trait Backend: Send {
    /// Decodes one packet. `Ok(None)` when it shows no picture.
    fn decode(&mut self, packet: &[u8]) -> Result<Option<Arc<Frame>>, String>;

    /// Called with each packet while the stream decodes in the call that submits it. A backend that decodes on the
    /// CPU, where one packet takes long and pictures come out late, answers with a [`Pipeline`] that a worker thread
    /// runs from then on (and `packet` goes to it). The default decodes in the calling thread.
    fn pipeline(&mut self, _packet: &[u8]) -> Option<Box<dyn Pipeline>> {
        None
    }
}

/// A decoded picture and the timestamp of the packet that it belongs to.
pub(crate) type Pic = (Arc<Frame>, i64);

/// A decoder that lags behind its input: a packet goes in, and its picture comes out later, after more packets or when
/// the caller waits for it. One worker thread owns it and calls it, never two at once.
pub(crate) trait Pipeline: Send {
    /// How many packets the decoder works on at once. It bounds the pictures that are still inside.
    fn lag(&self) -> usize;
    /// Sends one packet. The pictures that come out because of it go to `out`. Gives whether the decoder took the packet
    /// in: a packet it dropped (before the first sequence header, empty) is not in flight and no picture follows.
    fn push(&mut self, packet: &[u8], ts: i64, out: &mut Vec<Pic>) -> Result<bool, String>;
    /// Blocks until the oldest packet in flight is decoded and gives its picture to `out`. Gives none when nothing is in
    /// flight, and returns at once then.
    fn wait(&mut self, out: &mut Vec<Pic>) -> Result<(), String>;
    /// The end of the stream: every picture that is still inside, in order.
    fn finish(&mut self, out: &mut Vec<Pic>) -> Result<(), String> {
        let mut n = out.len();
        loop {
            self.wait(out)?;
            if out.len() == n {
                return Ok(());
            }
            n = out.len();
        }
    }
    /// Drops the pictures inside and the state of the stream.
    fn flush(&mut self);
}

/// Opens a decoder for a stream that starts with this key frame.
pub(crate) type Opener = fn(&KeyFrame) -> Result<Box<dyn Backend>, String>;

/// What the decoder needs from the host and the game.
pub(crate) trait Io: Send + Sync {
    /// Runs `job` on a guest thread after `delay`.
    fn defer(&self, delay: Duration, job: Box<dyn FnOnce() + Send>);
    /// Signals the event object at `event`. Called on a guest thread.
    fn signal(&self, event: u64);
    /// Whether `addr` can be an event object: aligned and readable.
    fn event_ok(&self, addr: u64) -> bool;
    /// Reads a word of guest memory.
    fn read_u64(&self, addr: u64) -> Option<u64>;
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum InSlot {
    Free,
    Held,
    Pending,
    /// Held by the game across a flush. It is handed out again after this time.
    Stale(Instant),
}

impl InSlot {
    fn usable(self, now: Instant) -> bool {
        match self {
            InSlot::Free => true,
            InSlot::Stale(t) => now >= t + STALE,
            InSlot::Held | InSlot::Pending => false,
        }
    }
}

struct Packet {
    index: usize,
    pts: i64,
    flags: u32,
    data: Vec<u8>,
}

#[derive(Default)]
struct Out {
    used: bool,
    cli: bool,
    rnd: bool,
    pts: i64,
    flags: u32,
    pic: Option<Arc<Frame>>,
}

struct State {
    started: bool,
    in_eos: bool,
    shutdown: bool,
    /// Counts the resets. Work and events of an older count are dropped.
    epoch: u64,
    /// Counts the resets that close the stream decoder.
    close_gen: u64,
    inputs: [InSlot; IN_COUNT],
    pending: VecDeque<Packet>,
    out: Vec<Out>,
    ready: VecDeque<usize>,
    fmt_sent: bool,
    width: u32,
    height: u32,
    colorspace: u32,
    tex_rel: bool,
    events: [u64; 4],
    events_ptr: u64,
    logged: u32,
}

impl State {
    fn reset(&mut self, close: bool) {
        self.epoch += 1;
        let now = Instant::now();
        for s in &mut self.inputs {
            *s = if *s == InSlot::Held {
                InSlot::Stale(now)
            } else {
                InSlot::Free
            };
        }
        self.pending.clear();
        self.ready.clear();
        self.in_eos = false;
        // A flush keeps the last picture of each slot: a game reads an output it acquired before the flush right after it.
        for o in &mut self.out {
            o.used = false;
            o.cli = false;
            o.rnd = false;
            if close {
                o.pic = None;
            }
        }
        if close {
            self.close_gen += 1;
        }
    }
}

struct Shared {
    state: Mutex<State>,
    cv: Condvar,
    io: Arc<dyn Io>,
    open: Opener,
    mem: Memory,
}

/// The picture format that `GetOutputFormat` reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Format {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) colorspace: u32,
}

/// One decoder object of the game.
pub(crate) struct Decoder {
    shared: Arc<Shared>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl Decoder {
    /// A decoder that opens its stream decoder with `open` at the first key frame.
    pub(crate) fn new(io: Arc<dyn Io>, open: Opener) -> Result<Decoder, String> {
        let state = State {
            started: false,
            in_eos: false,
            shutdown: false,
            epoch: 0,
            close_gen: 0,
            inputs: [InSlot::Free; IN_COUNT],
            pending: VecDeque::new(),
            out: (0..OUT_COUNT).map(|_| Out::default()).collect(),
            ready: VecDeque::new(),
            fmt_sent: false,
            width: 0,
            height: 0,
            colorspace: DEFAULT_COLORSPACE,
            tex_rel: false,
            events: [0; 4],
            events_ptr: 0,
            logged: 0,
        };
        let shared = Arc::new(Shared {
            state: Mutex::new(state),
            cv: Condvar::new(),
            io,
            open,
            mem: Memory::new(IN_COUNT * IN_CAP),
        });
        let for_thread = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("nxkit-video".to_string())
            .stack_size(2 << 20)
            .spawn(move || worker(&for_thread))
            .map_err(|e| format!("cannot start the video decoder thread: {e}"))?;
        Ok(Decoder {
            shared,
            worker: Mutex::new(Some(handle)),
        })
    }

    /// `Configure`: the size from the container, if no picture format went out yet.
    pub(crate) fn configure(&self, width: i32, height: i32) {
        let mut st = lock(&self.shared.state);
        if width > 0 && height > 0 && !st.fmt_sent {
            st.width = width as u32;
            st.height = height as u32;
        }
    }

    /// `RegisterClientEvents`: `entries` points at four event pointers (input, output, format, error), and each one
    /// that is an event now is remembered.
    pub(crate) fn register_events(&self, entries: u64) {
        let mut st = lock(&self.shared.state);
        st.events_ptr = entries;
        for i in 0..4 {
            let p = if entries == 0 {
                None
            } else {
                self.shared.io.read_u64(entries + 8 * i as u64)
            };
            st.events[i] = p.filter(|&p| self.shared.io.event_ok(p)).unwrap_or(0);
        }
    }

    /// `Start`.
    pub(crate) fn start(&self) {
        let mut st = lock(&self.shared.state);
        st.started = true;
        self.shared.schedule(&st, 0, START_DELAY);
        drop(st);
    }

    /// `Stop`: no event goes out after this, and the stream decoder is closed.
    pub(crate) fn stop(&self) {
        let mut st = lock(&self.shared.state);
        st.started = false;
        st.reset(true);
        drop(st);
        crate::pacing::stop();
    }

    /// `Reset`: closes the stream decoder and forgets the picture format.
    pub(crate) fn reset(&self) {
        let mut st = lock(&self.shared.state);
        st.reset(true);
        st.fmt_sent = false;
    }

    /// `Flush`: all inputs and outputs go back, the stream decoder stays open.
    pub(crate) fn flush(&self) {
        lock(&self.shared.state).reset(false);
    }

    /// `AcquireInputBufferIndex`: the index and how many inputs are still free, or the error code.
    pub(crate) fn acquire_input(&self) -> Result<(i32, i32), u32> {
        let mut st = lock(&self.shared.state);
        if !st.started || st.in_eos {
            return Err(ERR_NO_INPUT);
        }
        let now = Instant::now();
        match st.inputs.iter().position(|s| s.usable(now)) {
            Some(i) => {
                st.inputs[i] = InSlot::Held;
                let left = st.inputs.iter().filter(|s| s.usable(now)).count();
                Ok((i as i32, left as i32))
            }
            None => {
                let soonest = st
                    .inputs
                    .iter()
                    .filter_map(|s| {
                        if let InSlot::Stale(t) = s {
                            Some(*t + STALE)
                        } else {
                            None
                        }
                    })
                    .min();
                if let Some(t) = soonest {
                    self.shared
                        .schedule(&st, 0, t.saturating_duration_since(now));
                }
                Err(ERR_NO_INPUT)
            }
        }
    }

    /// The address of input buffer `index`, or `None` outside the range.
    pub(crate) fn input_addr(&self, index: i32) -> Option<u64> {
        if index < 0 || index as usize >= IN_COUNT {
            return None;
        }
        Some(self.shared.mem.ptr as u64 + (index as usize * IN_CAP) as u64)
    }

    /// `SendInputBufferForDecode`. Returns 0 or an error code.
    pub(crate) fn send_input(
        &self,
        index: i32,
        offset: i32,
        size: i32,
        pts: i64,
        flags: u32,
    ) -> u32 {
        if index < 0
            || index as usize >= IN_COUNT
            || offset < 0
            || size < 0
            || offset as usize + size as usize > IN_CAP
        {
            return ERR_BAD_ARG;
        }
        let i = index as usize;
        let mut st = lock(&self.shared.state);
        if st.inputs[i] == InSlot::Pending || st.pending.len() >= MAX_PENDING {
            if st.logged & 1 == 0 {
                st.logged |= 1;
                eprintln!(
                    "[RED] video: input {i} was sent while it or {MAX_PENDING} packets are still waiting for an output slot, the game sent more than the protocol allows"
                );
            }
            return ERR_BAD_ARG;
        }
        st.inputs[i] = InSlot::Pending;
        let eos = flags & FLAG_EOS != 0;
        if eos {
            st.in_eos = true;
        }
        // An end-of-stream packet without a timestamp or with a full buffer carries no picture.
        let size = if eos && (pts < 0 || size as usize >= IN_CAP) {
            0
        } else {
            size as usize
        };
        let data = self.shared.mem.read(i * IN_CAP + offset as usize, size);
        let key = parse_key_frame(&data);
        if let Some(k) = &key
            && !st.fmt_sent
        {
            if st.width == 0 || st.height == 0 {
                st.width = k.width;
                st.height = k.height;
            }
            st.colorspace = k.nv12_colorspace();
        }
        let first = !st.fmt_sent;
        if first {
            st.fmt_sent = true;
            if st.width == 0 || st.height == 0 {
                st.width = DEFAULT_WIDTH;
                st.height = DEFAULT_HEIGHT;
            }
        }
        st.pending.push_back(Packet {
            index: i,
            pts,
            flags,
            data,
        });
        self.shared.cv.notify_all();
        drop(st);
        if first {
            self.shared.signal_now(2);
        }
        0
    }

    /// `AcquireOutputBufferIndex`: the index, the timestamp, the flags and how many outputs are still waiting.
    pub(crate) fn acquire_output(&self) -> Result<(i32, i64, u32, i32), u32> {
        let mut st = lock(&self.shared.state);
        if !st.started {
            return Err(ERR_NO_OUTPUT);
        }
        let Some(s) = st.ready.pop_front() else {
            return Err(ERR_NO_OUTPUT);
        };
        st.out[s].cli = true;
        let left = st.ready.len() as i32;
        Ok((s as i32, st.out[s].pts, st.out[s].flags, left))
    }

    /// The format of the pictures.
    pub(crate) fn format(&self) -> Format {
        let st = lock(&self.shared.state);
        Format {
            width: if st.width == 0 {
                DEFAULT_WIDTH
            } else {
                st.width
            },
            height: if st.height == 0 {
                DEFAULT_HEIGHT
            } else {
                st.height
            },
            colorspace: st.colorspace,
        }
    }

    /// The picture of output slot `index`, which is `None` for an end-of-stream output.
    pub(crate) fn picture(&self, index: usize) -> Option<Arc<Frame>> {
        lock(&self.shared.state)
            .out
            .get(index)
            .and_then(|o| o.pic.clone())
    }

    /// A picture of slot `index` went into the game's texture. Slots that were rendered before and that the game does
    /// not release itself go back to the decoder. Returns how many.
    pub(crate) fn rendered(&self, index: usize) -> usize {
        let mut st = lock(&self.shared.state);
        let mut n = 0;
        if !st.tex_rel {
            for k in 0..OUT_COUNT {
                if k != index && st.out[k].rnd && st.out[k].cli {
                    st.out[k] = Out::default();
                    n += 1;
                }
            }
        }
        if let Some(o) = st.out.get_mut(index)
            && o.cli
        {
            o.rnd = true;
        }
        if n > 0 {
            self.shared.cv.notify_all();
        }
        n
    }

    /// `ReleaseOutputBufferIndex`. A slot that the game does not hold is left alone.
    pub(crate) fn release_output(&self, index: usize) {
        let mut st = lock(&self.shared.state);
        let Some(o) = st.out.get(index) else {
            return;
        };
        if !o.cli {
            return;
        }
        if o.rnd {
            st.tex_rel = true;
        }
        st.out[index] = Out::default();
        self.shared.cv.notify_all();
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        {
            let mut st = lock(&self.shared.state);
            st.shutdown = true;
            st.started = false;
            st.epoch += 1;
        }
        self.shared.cv.notify_all();
        if let Some(h) = lock(&self.worker).take() {
            let _ = h.join();
        }
        crate::pacing::stop();
    }
}

mod memory;
#[cfg(test)]
mod tests;
mod worker;

use memory::Memory;
pub(crate) use worker::decode_packet;
use worker::worker;
