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

/// The input memory that the game fills. The game writes to it, so it is raw memory and not a Rust slice.
struct Memory {
    ptr: *mut u8,
    len: usize,
}

// SAFETY: the memory is plain bytes that are allocated once and freed in `Drop`. All reads go through raw pointer
// copies.
unsafe impl Send for Memory {}
// SAFETY: as for `Send`.
unsafe impl Sync for Memory {}

impl Memory {
    fn new(len: usize) -> Memory {
        let slice: Box<[u8]> = vec![0u8; len].into_boxed_slice();
        Memory {
            ptr: Box::into_raw(slice).cast::<u8>(),
            len,
        }
    }

    fn read(&self, offset: usize, size: usize) -> Vec<u8> {
        let mut v = vec![0u8; size];
        if offset + size <= self.len {
            // SAFETY: the range is inside the allocation and `v` has `size` bytes. A write of the game at the same time
            // gives torn bytes, not undefined behaviour of the allocation.
            unsafe { std::ptr::copy_nonoverlapping(self.ptr.add(offset), v.as_mut_ptr(), size) };
        }
        v
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        // SAFETY: `ptr` and `len` come from `Box::into_raw` of a boxed slice in `new` and are freed only here.
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(self.ptr, self.len)) });
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
        if let Some(k) = &key {
            if !st.fmt_sent {
                if st.width == 0 || st.height == 0 {
                    st.width = k.width;
                    st.height = k.height;
                }
                st.colorspace = k.nv12_colorspace();
            }
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
        if let Some(o) = st.out.get_mut(index) {
            if o.cli {
                o.rnd = true;
            }
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

impl Shared {
    /// The event object for client event `i`. The client's struct is read again each time: a game registers it before
    /// it fills it, and the entry that was an event at registration is the fallback.
    fn event(&self, st: &State, i: usize) -> Option<u64> {
        let live = if st.events_ptr == 0 {
            None
        } else {
            self.io.read_u64(st.events_ptr + 8 * i as u64)
        };
        match live {
            Some(p) if self.io.event_ok(p) => Some(p),
            _ => Some(st.events[i]).filter(|&p| p != 0),
        }
    }

    /// Signals client event `i` after `delay` on a guest thread, unless the decoder was stopped or reset by then.
    fn schedule(self: &Arc<Shared>, st: &State, i: usize, delay: Duration) {
        let epoch = st.epoch;
        let me = Arc::clone(self);
        self.io.defer(
            delay,
            Box::new(move || {
                let st = lock(&me.state);
                if st.shutdown || !st.started || st.epoch != epoch {
                    return;
                }
                // The lock is held while the event is signalled, so a stop never interleaves with a signal.
                if let Some(p) = me.event(&st, i) {
                    me.io.signal(p);
                }
            }),
        );
    }

    /// Signals client event `i` now. The caller is on a guest thread and holds no lock.
    fn signal_now(&self, i: usize) {
        let p = {
            let st = lock(&self.state);
            self.event(&st, i)
        };
        if let Some(p) = p {
            self.io.signal(p);
        }
    }
}

pub(crate) fn decode_packet(
    backend: &mut Option<(Box<dyn Backend>, u32, u32)>,
    open: Opener,
    data: &[u8],
) -> Result<Option<Arc<Frame>>, String> {
    if data.is_empty() {
        return Ok(None);
    }
    if let Some(k) = parse_key_frame(data) {
        let same = matches!(backend, Some((_, w, h)) if *w == k.width && *h == k.height);
        if !same {
            *backend = None;
            *backend = Some((open(&k)?, k.width, k.height));
        }
    }
    match backend {
        Some((b, _, _)) => b.decode(data),
        // A stream cannot start in the middle: packets before the first key frame show nothing.
        None => Ok(None),
    }
}

/// Decodes one packet of an AV1 stream. The decoder opens at the first packet and finds the sequence header itself.
pub(crate) fn decode_av1(
    backend: &mut Option<(Box<dyn Backend>, u32, u32)>,
    open: fn() -> Result<Box<dyn Backend>, String>,
    data: &[u8],
) -> Result<Option<Arc<Frame>>, String> {
    if data.is_empty() {
        return Ok(None);
    }
    if backend.is_none() {
        *backend = Some((open()?, 0, 0));
    }
    match backend {
        Some((b, _, _)) => b.decode(data),
        None => Ok(None),
    }
}

fn worker(sh: &Arc<Shared>) {
    let mut backend: Option<(Box<dyn Backend>, u32, u32)> = None;
    let mut seen_close = 0;
    let mut st = lock(&sh.state);
    loop {
        if st.shutdown {
            return;
        }
        let Some(slot) = st.out.iter().position(|o| !o.used) else {
            st = sh.cv.wait(st).unwrap_or_else(PoisonError::into_inner);
            continue;
        };
        let Some(packet) = st.pending.pop_front() else {
            st = sh.cv.wait(st).unwrap_or_else(PoisonError::into_inner);
            continue;
        };
        st.out[slot].used = true;
        let epoch = st.epoch;
        let close = st.close_gen;
        drop(st);

        if close != seen_close {
            backend = None;
            seen_close = close;
        }
        let result = decode_packet(&mut backend, sh.open, &packet.data);

        st = lock(&sh.state);
        if st.shutdown {
            return;
        }
        if st.epoch != epoch {
            // A reset put every slot back while this packet was decoding.
            continue;
        }
        st.inputs[packet.index] = InSlot::Free;
        let pic = match result {
            Ok(p) => p,
            Err(e) => {
                if st.logged & 2 == 0 {
                    st.logged |= 2;
                    eprintln!("[RED] video: a packet was not decoded: {e}");
                }
                None
            }
        };
        let has_pic = pic.is_some();
        if let Some(f) = pic {
            st.colorspace = f.colorspace();
            st.out[slot].pic = Some(f);
        }
        let ready = has_pic || packet.flags & FLAG_EOS != 0;
        if ready {
            st.out[slot].pts = packet.pts;
            st.out[slot].flags = packet.flags;
            st.ready.push_back(slot);
        } else {
            st.out[slot].used = false;
        }
        if !st.in_eos {
            sh.schedule(&st, 0, Duration::ZERO);
        }
        if ready {
            sh.schedule(&st, 1, Duration::ZERO);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Nv12;
    use crate::vp9::key_frame_bytes;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockIo {
        signals: Mutex<Vec<u64>>,
        jobs: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
        defers: AtomicUsize,
    }

    impl MockIo {
        fn new() -> Arc<MockIo> {
            Arc::new(MockIo {
                signals: Mutex::new(vec![]),
                jobs: Mutex::new(vec![]),
                defers: AtomicUsize::new(0),
            })
        }

        fn run_jobs(&self) {
            let jobs: Vec<_> = std::mem::take(&mut *lock(&self.jobs));
            for j in jobs {
                j();
            }
        }
    }

    impl Io for MockIo {
        fn defer(&self, _delay: Duration, job: Box<dyn FnOnce() + Send>) {
            self.defers.fetch_add(1, Ordering::SeqCst);
            lock(&self.jobs).push(job);
        }
        fn signal(&self, event: u64) {
            lock(&self.signals).push(event);
        }
        fn event_ok(&self, addr: u64) -> bool {
            addr != 0
        }
        fn read_u64(&self, addr: u64) -> Option<u64> {
            Some(addr)
        }
    }

    struct MockBackend;

    impl Backend for MockBackend {
        fn decode(&mut self, _packet: &[u8]) -> Result<Option<Arc<Frame>>, String> {
            let planes = Nv12 {
                y: vec![0; 16 * 16],
                y_stride: 16,
                uv: vec![128; 16 * 8],
                uv_stride: 16,
            };
            Ok(Some(Arc::new(Frame::from_nv12(16, 16, 3, planes))))
        }
    }

    fn open(_key: &KeyFrame) -> Result<Box<dyn Backend>, String> {
        Ok(Box::new(MockBackend))
    }

    fn decoder() -> (Decoder, Arc<MockIo>) {
        let io = MockIo::new();
        let d = Decoder::new(io.clone(), open).unwrap_or_else(|e| unreachable!("{e}"));
        (d, io)
    }

    fn put(d: &Decoder, index: i32, bytes: &[u8]) {
        let addr = d.input_addr(index).unwrap_or_else(|| unreachable!());
        // SAFETY: the address is inside the input memory of the decoder and the bytes fit in one buffer.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), addr as *mut u8, bytes.len()) };
    }

    fn wait_output(d: &Decoder) -> (i32, i64, u32, i32) {
        for _ in 0..400 {
            if let Ok(o) = d.acquire_output() {
                return o;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        unreachable!("no output within two seconds")
    }

    fn send_key(d: &Decoder, pts: i64, flags: u32) -> i32 {
        let (i, _) = d.acquire_input().unwrap_or_else(|e| unreachable!("{e:#x}"));
        let bytes = key_frame_bytes(2, 0, 1920, 1080);
        put(d, i, &bytes);
        assert_eq!(d.send_input(i, 0, bytes.len() as i32, pts, flags), 0);
        i
    }

    #[test]
    fn a_packet_comes_back_as_an_output_with_its_timestamp() {
        let (d, io) = decoder();
        d.register_events(0x100);
        d.start();
        send_key(&d, 1234, 0);
        let (idx, pts, flags, left) = wait_output(&d);
        assert_eq!((pts, flags, left), (1234, 0, 0));
        assert!(d.picture(idx as usize).is_some());
        assert_eq!(
            d.format(),
            Format {
                width: 1920,
                height: 1080,
                colorspace: 3
            }
        );
        io.run_jobs();
        assert!(!lock(&io.signals).is_empty());
    }

    #[test]
    fn nothing_is_handed_out_before_start() {
        let (d, _io) = decoder();
        assert_eq!(d.acquire_input(), Err(ERR_NO_INPUT));
        assert_eq!(d.acquire_output(), Err(ERR_NO_OUTPUT));
    }

    #[test]
    fn an_end_of_stream_packet_without_a_picture_still_gives_an_output() {
        let (d, _io) = decoder();
        d.start();
        let (i, _) = d.acquire_input().unwrap_or_else(|e| unreachable!("{e:#x}"));
        assert_eq!(d.send_input(i, 0, 0, 0, FLAG_EOS), 0);
        let (idx, _, flags, _) = wait_output(&d);
        assert_eq!(flags, FLAG_EOS);
        assert!(d.picture(idx as usize).is_none());
        assert_eq!(d.acquire_input(), Err(ERR_NO_INPUT));
    }

    #[test]
    fn a_stop_drops_the_events_that_were_waiting() {
        let (d, io) = decoder();
        d.register_events(0x100);
        d.start();
        d.stop();
        io.run_jobs();
        assert!(lock(&io.signals).is_empty());
    }

    #[test]
    fn an_input_held_across_a_flush_waits_before_it_comes_again() {
        let (d, _io) = decoder();
        d.start();
        let (i, _) = d.acquire_input().unwrap_or_else(|e| unreachable!("{e:#x}"));
        d.flush();
        let (j, left) = d.acquire_input().unwrap_or_else(|e| unreachable!("{e:#x}"));
        assert_ne!(i, j);
        assert_eq!(left, 2);
    }

    #[test]
    fn bad_arguments_are_refused() {
        let (d, _io) = decoder();
        d.start();
        assert_eq!(d.send_input(-1, 0, 0, 0, 0), ERR_BAD_ARG);
        assert_eq!(d.send_input(0, 0, IN_CAP as i32 + 1, 0, 0), ERR_BAD_ARG);
        assert_eq!(d.send_input(IN_COUNT as i32, 0, 0, 0, 0), ERR_BAD_ARG);
    }

    #[test]
    fn a_released_slot_lets_the_decoder_go_on() {
        let (d, _io) = decoder();
        d.start();
        let mut held = vec![];
        for n in 0..OUT_COUNT {
            send_key(&d, n as i64, 0);
            held.push(wait_output(&d).0);
        }
        send_key(&d, 99, 0);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(d.acquire_output(), Err(ERR_NO_OUTPUT));
        d.release_output(held[0] as usize);
        assert_eq!(wait_output(&d).1, 99);
    }
}
