//! The mixer. One output unit plays every open output (AudioOut, OutputStream, the renderer's device sink).
//!
//! The device callback runs [`Mixer::render`]. It takes no lock and allocates nothing. The game side and the callback meet
//! in two lock-free queues per output: the buffers to play and the buffers that were played. A third thread, the notifier,
//! signals the release events, because the callback must not call into the kernel.

use crate::host;
use crate::sys::au;
use crate::sys::mem;
use crate::sys::ring::Ring;
use crate::sys::sem::Sem;
use crate::sys::vdsp;
use nxkit_abi::{guest, kernel};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Open outputs at the same time.
pub(crate) const SLOTS: usize = 8;
/// Buffers in flight per output.
pub(crate) const QMAX: usize = 64;
/// The state value of a started output.
pub(crate) const STARTED: u32 = 0;
/// The state value of a stopped output.
pub(crate) const STOPPED: u32 = 1;
/// The sample rate of every output.
pub(crate) const RATE: u32 = 48000;
const CHUNK: usize = 256;

/// One buffer of PCM16 samples. The data stays in guest memory.
#[derive(Clone, Copy, Default)]
pub(crate) struct Buf {
    /// What the caller wants back: the guest address of the `AudioOutBuffer`, or the client tag of the IPC call.
    pub token: u64,
    /// The guest address of the samples.
    pub addr: u64,
    /// The size in bytes.
    pub size: u64,
    /// True when `addr` is a block of the host and not guest memory.
    pub host: bool,
}

/// What to signal when buffers were played.
#[derive(Clone, Copy)]
pub(crate) enum Notify {
    /// Nothing. The game polls.
    None,
    /// The guest `SystemEvent` at this address.
    Guest(u64),
    /// The writable end of a kernel event.
    Kernel(u32),
    /// A semaphore of a host thread that waits for a played buffer.
    Wake(&'static Sem),
}

struct Slot {
    open: AtomicBool,
    state: AtomicU32,
    channels: AtomicU32,
    vol: AtomicU32,
    /// Held by whoever consumes `pending` and produces `released`: the callback, or a flush.
    busy: AtomicBool,
    pending: Ring<Buf>,
    released: Ring<Buf>,
    queued: AtomicU32,
    played: AtomicU64,
    rel_flag: AtomicBool,
    underruns: AtomicU64,
    cur_valid: AtomicBool,
    cur_token: AtomicU64,
    cur_addr: AtomicU64,
    cur_size: AtomicU64,
    cur_host: AtomicBool,
    cur_pos: AtomicU64,
    /// The tokens of the buffers in flight, in append order. A buffer is released in append order too, so the ones in
    /// flight are the entries from `rel_seq` up to `app_seq`. Only `contains` reads them.
    tokens: [AtomicU64; QMAX],
    /// Buffers appended. Written by the game side that appends.
    app_seq: AtomicU64,
    /// Buffers taken back. Written by the game side that polls the released buffers.
    rel_seq: AtomicU64,
    notify: Mutex<Notify>,
}

impl Slot {
    fn new() -> Slot {
        Slot {
            open: AtomicBool::new(false),
            state: AtomicU32::new(STOPPED),
            channels: AtomicU32::new(2),
            vol: AtomicU32::new(1.0f32.to_bits()),
            busy: AtomicBool::new(false),
            pending: Ring::new(QMAX),
            released: Ring::new(QMAX),
            queued: AtomicU32::new(0),
            played: AtomicU64::new(0),
            rel_flag: AtomicBool::new(false),
            underruns: AtomicU64::new(0),
            cur_valid: AtomicBool::new(false),
            cur_token: AtomicU64::new(0),
            cur_addr: AtomicU64::new(0),
            cur_size: AtomicU64::new(0),
            cur_host: AtomicBool::new(false),
            cur_pos: AtomicU64::new(0),
            tokens: [const { AtomicU64::new(0) }; QMAX],
            app_seq: AtomicU64::new(0),
            rel_seq: AtomicU64::new(0),
            notify: Mutex::new(Notify::None),
        }
    }

    fn lock_busy(&self) {
        while self
            .busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            std::thread::yield_now();
        }
    }

    fn unlock_busy(&self) {
        self.busy.store(false, Ordering::Release);
    }

    /// Moves the playing buffer and every pending buffer to `released`. Needs `busy`.
    fn drain_to_released(&self) -> bool {
        let mut any = false;
        if self.cur_valid.swap(false, Ordering::Relaxed) {
            let b = Buf {
                token: self.cur_token.load(Ordering::Relaxed),
                addr: self.cur_addr.load(Ordering::Relaxed),
                size: self.cur_size.load(Ordering::Relaxed),
                host: self.cur_host.load(Ordering::Relaxed),
            };
            self.cur_pos.store(0, Ordering::Relaxed);
            any |= self.released.push(b);
        }
        while let Some(b) = self.pending.pop() {
            any |= self.released.push(b);
        }
        self.queued.store(0, Ordering::Relaxed);
        any
    }
}

enum OutState {
    Unstarted,
    Device(au::Output),
    Silent,
}

/// The mixer. There is one per process.
pub(crate) struct Mixer {
    slots: Vec<Slot>,
    sem: Sem,
    master: AtomicU32,
    faults: AtomicU64,
    alloc: Mutex<()>,
    out: Mutex<OutState>,
    force_silent: AtomicBool,
    notifier: Mutex<bool>,
}

static MIXER: OnceLock<Arc<Mixer>> = OnceLock::new();

/// The mixer of the process.
pub(crate) fn mixer() -> &'static Arc<Mixer> {
    MIXER.get_or_init(|| {
        Arc::new(Mixer {
            slots: (0..SLOTS).map(|_| Slot::new()).collect(),
            sem: Sem::new(),
            master: AtomicU32::new(1.0f32.to_bits()),
            faults: AtomicU64::new(0),
            alloc: Mutex::new(()),
            out: Mutex::new(OutState::Unstarted),
            force_silent: AtomicBool::new(false),
            notifier: Mutex::new(false),
        })
    })
}

/// Reads `ch` interleaved PCM16 channels per frame from `bytes` into stereo `dst`, times `gain`.
#[allow(
    clippy::approx_constant,
    reason = "0.7071 is the default downmix coefficient of the SDK, not 1/sqrt(2) to full precision"
)]
fn convert(bytes: &[u8], ch: usize, gain: f32, dst: &mut [f32]) {
    let rd =
        |f: &[u8], j: usize| -> f32 { f32::from(i16::from_le_bytes([f[2 * j], f[2 * j + 1]])) };
    let frame = 2 * ch.max(1);
    for (i, f) in bytes.chunks_exact(frame).enumerate() {
        let (l, r) = match ch {
            1 => (rd(f, 0), rd(f, 0)),
            6 => {
                // front left, front right, center, LFE, back left, back right, with the default downmix coefficients
                let c = rd(f, 2) * 0.7071 + rd(f, 3) * 0.2512;
                (
                    rd(f, 0) + c + rd(f, 4) * 0.7071,
                    rd(f, 1) + c + rd(f, 5) * 0.7071,
                )
            }
            _ => (rd(f, 0), rd(f, 1)),
        };
        dst[2 * i] = l * gain;
        dst[2 * i + 1] = r * gain;
    }
}

impl Mixer {
    /// Sets the master gain and whether to skip the device.
    pub(crate) fn configure(&self, master: f32, silent: bool) {
        self.master
            .store(master.max(0.0).to_bits(), Ordering::Relaxed);
        self.force_silent.store(silent, Ordering::Relaxed);
    }

    /// Opens an output. Returns its index, or `None` when all slots are in use.
    pub(crate) fn open(&self, channels: u32, notify: Notify) -> Option<usize> {
        let _g = self.alloc.lock().ok()?;
        let idx = self
            .slots
            .iter()
            .position(|s| !s.open.load(Ordering::Acquire))?;
        let s = &self.slots[idx];
        s.lock_busy();
        s.drain_to_released();
        while s.released.pop().is_some() {}
        s.unlock_busy();
        s.state.store(STOPPED, Ordering::Relaxed);
        s.channels.store(channels, Ordering::Relaxed);
        s.vol.store(1.0f32.to_bits(), Ordering::Relaxed);
        s.played.store(0, Ordering::Relaxed);
        s.rel_flag.store(false, Ordering::Relaxed);
        s.underruns.store(0, Ordering::Relaxed);
        s.app_seq.store(0, Ordering::Relaxed);
        s.rel_seq.store(0, Ordering::Relaxed);
        if let Ok(mut n) = s.notify.lock() {
            *n = notify;
        }
        s.open.store(true, Ordering::Release);
        self.ensure_notifier();
        Some(idx)
    }

    /// Stops the output, gives every buffer back and frees the slot.
    pub(crate) fn close(&self, idx: usize) {
        let Some(s) = self.slots.get(idx) else { return };
        self.stop(idx);
        let _g = self.alloc.lock();
        s.lock_busy();
        s.open.store(false, Ordering::Release);
        s.unlock_busy();
    }

    /// Starts playing. The first start opens the device.
    pub(crate) fn start(&self, idx: usize) {
        let Some(s) = self.slots.get(idx) else { return };
        s.state.store(STARTED, Ordering::Release);
        self.ensure_output();
    }

    /// Stops playing. Every buffer, also a part played one, comes back as released.
    pub(crate) fn stop(&self, idx: usize) {
        let Some(s) = self.slots.get(idx) else { return };
        let was = s.state.swap(STOPPED, Ordering::AcqRel);
        s.lock_busy();
        let any = s.drain_to_released();
        s.unlock_busy();
        if was == STARTED && any {
            s.rel_flag.store(true, Ordering::Release);
            self.sem.signal();
        }
    }

    /// Gives back every buffer that was not played, as `stop` does, but keeps the state.
    pub(crate) fn purge(&self, idx: usize) {
        let Some(s) = self.slots.get(idx) else { return };
        s.lock_busy();
        let any = s.drain_to_released();
        s.unlock_busy();
        if any {
            s.rel_flag.store(true, Ordering::Release);
            self.sem.signal();
        }
    }

    /// The state: `STARTED` or `STOPPED`.
    pub(crate) fn state(&self, idx: usize) -> u32 {
        self.slots
            .get(idx)
            .map_or(STOPPED, |s| s.state.load(Ordering::Acquire))
    }

    /// The channel count of the output.
    pub(crate) fn channels(&self, idx: usize) -> u32 {
        self.slots
            .get(idx)
            .map_or(2, |s| s.channels.load(Ordering::Relaxed))
    }

    /// Queues a buffer. Returns false when the queue is full. One game thread at a time appends to an output, as the
    /// SDK's `AppendAudioOutBuffer` requires, so the queue needs no lock.
    pub(crate) fn append(&self, idx: usize, b: Buf) -> bool {
        let Some(s) = self.slots.get(idx) else {
            return false;
        };
        let n = s.app_seq.load(Ordering::Relaxed);
        s.tokens[(n % QMAX as u64) as usize].store(b.token, Ordering::Release);
        if !s.pending.push(b) {
            return false;
        }
        s.app_seq.store(n + 1, Ordering::Release);
        s.queued.fetch_add(1, Ordering::AcqRel);
        true
    }

    /// Takes the oldest played buffer. One game thread at a time polls an output.
    pub(crate) fn released(&self, idx: usize) -> Option<Buf> {
        let s = self.slots.get(idx)?;
        let b = s.released.pop()?;
        s.rel_seq.fetch_add(1, Ordering::AcqRel);
        Some(b)
    }

    /// Whether a buffer with this token is in flight. It scans at most `QMAX` entries and is only for a game that asks.
    pub(crate) fn contains(&self, idx: usize, token: u64) -> bool {
        let Some(s) = self.slots.get(idx) else {
            return false;
        };
        let rel = s.rel_seq.load(Ordering::Acquire);
        let n = s
            .app_seq
            .load(Ordering::Acquire)
            .saturating_sub(rel)
            .min(QMAX as u64);
        (0..n)
            .any(|k| s.tokens[((rel + k) % QMAX as u64) as usize].load(Ordering::Acquire) == token)
    }

    /// Buffers appended and not yet played to the end, the playing one included.
    pub(crate) fn queued(&self, idx: usize) -> u32 {
        self.slots
            .get(idx)
            .map_or(0, |s| s.queued.load(Ordering::Acquire))
    }

    /// Frames played since the open.
    pub(crate) fn played(&self, idx: usize) -> u64 {
        self.slots
            .get(idx)
            .map_or(0, |s| s.played.load(Ordering::Relaxed))
    }

    /// Sets the gain of the output.
    pub(crate) fn set_volume(&self, idx: usize, v: f32) {
        if let Some(s) = self.slots.get(idx) {
            let v = if v >= 0.0 { v } else { 0.0 };
            s.vol.store(v.to_bits(), Ordering::Relaxed);
        }
    }

    /// The gain of the output.
    pub(crate) fn volume(&self, idx: usize) -> f32 {
        self.slots
            .get(idx)
            .map_or(1.0, |s| f32::from_bits(s.vol.load(Ordering::Relaxed)))
    }

    /// Fills `out` (interleaved stereo) with the sum of the started outputs. Runs on the device thread: no lock, no
    /// allocation.
    pub(crate) fn render(&self, out: &mut [f32]) {
        let master = f32::from_bits(self.master.load(Ordering::Relaxed));
        let mut scratch = [0.0f32; CHUNK * 2];
        for chunk in out.chunks_mut(CHUNK * 2) {
            chunk.fill(0.0);
            for s in &self.slots {
                self.mix_slot(s, chunk, &mut scratch);
            }
            if master != 1.0 {
                vdsp::scale(chunk, master);
            }
            vdsp::clip(chunk, -1.0, 1.0);
        }
    }

    fn mix_slot(&self, s: &Slot, chunk: &mut [f32], scratch: &mut [f32; CHUNK * 2]) {
        if !s.open.load(Ordering::Acquire) || s.state.load(Ordering::Acquire) != STARTED {
            return;
        }
        if s.busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        let n = chunk.len();
        let frames = n / 2;
        scratch[..n].fill(0.0);
        let ch = s.channels.load(Ordering::Relaxed) as usize;
        let fb = (2 * ch.max(1)) as u64;
        let gain = f32::from_bits(s.vol.load(Ordering::Relaxed)) / 32768.0;
        let mut done = 0usize;
        let mut any_released = false;
        while done < frames {
            if !s.cur_valid.load(Ordering::Relaxed) {
                let Some(b) = s.pending.pop() else {
                    s.underruns.fetch_add(1, Ordering::Relaxed);
                    break;
                };
                s.cur_token.store(b.token, Ordering::Relaxed);
                s.cur_addr.store(b.addr, Ordering::Relaxed);
                s.cur_size.store(b.size, Ordering::Relaxed);
                s.cur_host.store(b.host, Ordering::Relaxed);
                s.cur_pos.store(0, Ordering::Relaxed);
                s.cur_valid.store(true, Ordering::Relaxed);
            }
            let addr = s.cur_addr.load(Ordering::Relaxed);
            let size = s.cur_size.load(Ordering::Relaxed);
            let host = s.cur_host.load(Ordering::Relaxed);
            let pos = s.cur_pos.load(Ordering::Relaxed);
            let avail = size.saturating_sub(pos) / fb;
            let mut finished = avail == 0;
            if !finished {
                let k = (avail as usize).min(frames - done);
                let dst = &mut scratch[done * 2..(done + k) * 2];
                let len = k as u64 * fb;
                let ok = if host {
                    mem::with_host(addr + pos, len, |bytes| convert(bytes, ch, gain, dst));
                    true
                } else {
                    guest::with_slice(addr + pos, len, |bytes| convert(bytes, ch, gain, dst))
                        .is_ok()
                };
                if ok {
                    s.cur_pos.store(pos + k as u64 * fb, Ordering::Relaxed);
                    s.played.fetch_add(k as u64, Ordering::Relaxed);
                    done += k;
                    finished = pos + k as u64 * fb + fb > size;
                } else {
                    self.faults.fetch_add(1, Ordering::Relaxed);
                    finished = true;
                }
            }
            if finished {
                let b = Buf {
                    token: s.cur_token.load(Ordering::Relaxed),
                    addr,
                    size,
                    host,
                };
                s.cur_valid.store(false, Ordering::Relaxed);
                s.queued.fetch_sub(1, Ordering::AcqRel);
                if !s.released.push(b) {
                    self.faults.fetch_add(1, Ordering::Relaxed);
                }
                s.rel_flag.store(true, Ordering::Release);
                any_released = true;
            }
        }
        vdsp::madd(chunk, &scratch[..n], 1.0);
        s.busy.store(false, Ordering::Release);
        if any_released {
            self.sem.signal();
        }
    }

    fn ensure_output(&self) {
        let Ok(mut out) = self.out.lock() else { return };
        if !matches!(*out, OutState::Unstarted) {
            return;
        }
        if self.force_silent.load(Ordering::Relaxed) {
            eprintln!(
                "nxkit-audio: the device is off by configuration, a silent sink paces the outputs"
            );
            self.spawn_silent();
            *out = OutState::Silent;
            return;
        }
        let m = Arc::clone(mixer());
        match au::Output::start(Box::new(move |buf| m.render(buf))) {
            Ok(o) => {
                eprintln!("nxkit-audio: the output unit runs at 48000 Hz stereo");
                *out = OutState::Device(o);
            }
            Err(e) => {
                eprintln!(
                    "nxkit-audio: [RED] the output unit did not start ({e}), a silent sink paces the outputs"
                );
                self.spawn_silent();
                *out = OutState::Silent;
            }
        }
    }

    fn spawn_silent(&self) {
        let m = Arc::clone(mixer());
        let r = std::thread::Builder::new()
            .name("nxkit-audio-silent".into())
            .spawn::<_, ()>(move || {
                let mut buf = vec![0.0f32; 480 * 2];
                let step = Duration::from_millis(10);
                let mut next = Instant::now() + step;
                loop {
                    m.render(&mut buf);
                    let now = Instant::now();
                    if next > now {
                        std::thread::sleep(next - now);
                    }
                    next += step;
                }
            });
        if let Err(e) = r {
            host::fatal(&format!("the silent sink thread did not start: {e}"));
        }
    }

    fn ensure_notifier(&self) {
        let Ok(mut started) = self.notifier.lock() else {
            return;
        };
        if *started {
            return;
        }
        let m = Arc::clone(mixer());
        match std::thread::Builder::new()
            .name("nxkit-audio-notify".into())
            .spawn(move || m.notify_loop())
        {
            Ok(_) => *started = true,
            Err(e) => host::fatal(&format!("the audio notifier thread did not start: {e}")),
        }
    }

    fn notify_loop(&self) {
        let mut reported = [0u64; SLOTS];
        let mut join_logged = false;
        let mut faults_seen = 0u64;
        loop {
            self.sem.wait_ms(20);
            for (i, s) in self.slots.iter().enumerate() {
                if s.rel_flag.swap(false, Ordering::AcqRel) {
                    let n = s.notify.lock().map_or(Notify::None, |g| *g);
                    match n {
                        Notify::None => {}
                        Notify::Guest(ev) => host::signal_event(ev),
                        Notify::Wake(w) => w.signal(),
                        Notify::Kernel(h) => {
                            if let Some(k) = kernel::kernel() {
                                let _ = k.signal_event(h);
                            }
                        }
                    }
                }
                let u = s.underruns.load(Ordering::Relaxed);
                if u > 0 && (reported[i] == 0 || u >= reported[i].saturating_mul(10)) {
                    reported[i] = u;
                    eprintln!(
                        "nxkit-audio: output {i} ran dry {u} times (the game did not append in time)"
                    );
                }
            }
            let f = self.faults.load(Ordering::Relaxed);
            if f != faults_seen {
                faults_seen = f;
                eprintln!(
                    "nxkit-audio: [RED] {f} buffers pointed outside guest memory or overflowed the released queue"
                );
            }
            if !join_logged
                && let Ok(out) = self.out.lock()
                && let OutState::Device(o) = &*out
                && let Some(st) = o.join_status()
            {
                join_logged = true;
                if st == 0 {
                    eprintln!(
                        "nxkit-audio: the audio thread joined the os_workgroup of the device"
                    );
                } else {
                    eprintln!(
                        "nxkit-audio: [RED] os_workgroup_join returned {st}, the audio thread is outside the workgroup"
                    );
                }
            }
        }
    }
}
