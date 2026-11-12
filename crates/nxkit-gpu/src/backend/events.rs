//! The GPU event signals (`nvnCommandBufferSignalEvent`) in guest memory.
//!
//! On the console the GPU writes the counter of an event when it reaches the signal, after the work before it is done,
//! and a game polls the counter (`nvnEventGetValue`) to know that the results of that work are ready. The walk turns
//! each signal of a frame into an event report ([`CounterReport::is_event`]) with the host address of the counter word,
//! taken when the walk reads the command (the console binds it when the command is recorded). The backend keeps the
//! reports of a frame until the GPU finished its batch and then writes them, in the order of the frames and of the
//! commands:
//!
//! - `NVN_EVENT_SIGNAL_MODE_ADD` adds the value with an atomic compare-and-swap, so a CPU store between the load and
//!   the swap is not overwritten (the swap fails and is tried again).
//! - Every other mode writes the value with an atomic swap.
//!
//! The write runs in the completion of the batch ([`SubmittedBatch::on_complete`]), after the completion of the private
//! GPU memory (registered before it), and before a wait for the batch returns. It marks the thread internal like the
//! writer of the counter reports, so under `NXKIT_PRIVATE` the page tracker serves it without a wait. A frame without
//! a batch is written at once. The gate (`nxkit-rt` `ctx::gate`) still keeps the counters that the signals leave when
//! their work goes into a frame: it orders the queues on the CPU timeline, before the GPU ran anything.
//!
//! The order with CPU signals: the GPU writes at the completion, which is the time the CPU sees the GPU reach the
//! signal, so a CPU signal made before it is older and the GPU value wins, as on the console when the GPU executes the
//! signal after the CPU wrote. Only a wait that returned before the frame was done (the early wait of
//! `NXKIT_PRIVATE=1`, the overlap) tells the CPU that the GPU passed the signal when it did not: a CPU signal after
//! such a wait is newer on the console. A write of the GPU from a frame up to the newest fence a CPU wait returned
//! for, at the time of a CPU signal of the same word, is dropped; an add of the GPU is still added (both adds count on
//! the console in either order).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use nxkit_metal::{GuestSpace, SubmittedBatch};

use super::locked;
use crate::detail::{CounterReport, REPORT_EVENT_ADD};

/// The event signals of the frames that the GPU has not finished.
pub(super) struct EventSignals {
    guest: GuestSpace,
    state: Mutex<State>,
    /// The newest fence that a CPU wait returned for (it may not be done on the GPU).
    waited: AtomicU64,
    /// The newest fence whose signals were written.
    applied: AtomicU64,
}

#[derive(Default)]
struct State {
    /// The signals of each frame, by fence, oldest first.
    frames: VecDeque<(u64, Vec<CounterReport>)>,
    /// The counter words that the CPU signalled after a wait, with the fence that wait returned for: a GPU write from a
    /// frame up to it is older than the CPU's.
    cpu: HashMap<u64, u64>,
    stats: Stats,
}

/// The counts of `NXKIT_DIAG=events` since the last line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Stats {
    pub(super) writes: u64,
    pub(super) adds: u64,
    /// GPU writes that a newer CPU signal replaced.
    pub(super) dropped: u64,
    /// Counter words outside guest memory or not aligned.
    pub(super) refused: u64,
}

impl EventSignals {
    pub(super) fn new(guest: GuestSpace) -> Arc<Self> {
        Arc::new(Self {
            guest,
            state: Mutex::new(State::default()),
            waited: AtomicU64::new(0),
            applied: AtomicU64::new(0),
        })
    }

    /// Keeps the event signals among `reports` of the frame of `fence` and writes them when `batch` is complete; at
    /// once without a batch. A frame without signals costs a scan of its reports.
    pub(super) fn hold(self: &Arc<Self>, fence: u64, reports: &[CounterReport], batch: Option<&SubmittedBatch>) {
        let signals: Vec<CounterReport> = reports.iter().filter(|r| r.is_event()).copied().collect();
        if signals.is_empty() {
            return;
        }
        locked(&self.state).frames.push_back((fence, signals));
        let this = self.clone();
        let done = move || this.complete(fence);
        match batch {
            Some(b) => b.on_complete(done),
            None => done(),
        }
    }

    /// A CPU wait for `fence` returned.
    pub(super) fn waited(&self, fence: u64) {
        self.waited.fetch_max(fence, Ordering::AcqRel);
    }

    /// The CPU signalled the counter at `word` (`nvnEventSignal`). Costs two loads when no wait returned early.
    pub(super) fn cpu_signalled(&self, word: u64) {
        let waited = self.waited.load(Ordering::Acquire);
        if waited <= self.applied.load(Ordering::Acquire) {
            return;
        }
        let mut st = locked(&self.state);
        let mark = st.cpu.entry(word).or_insert(0);
        *mark = (*mark).max(waited);
    }

    /// The GPU finished the frame of `fence`, and with it every frame before it: their signals go into guest memory.
    pub(super) fn complete(&self, fence: u64) {
        // The page tracker serves the writes of this thread without a wait, as those of the counter reports.
        let _internal = nxkit_metal::mark_internal_thread();
        let mut st = locked(&self.state);
        while st.frames.front().is_some_and(|(f, _)| *f <= fence) {
            let Some((f, signals)) = st.frames.pop_front() else {
                break;
            };
            for s in &signals {
                let add = s.kind == REPORT_EVENT_ADD;
                if !add && st.cpu.get(&s.addr).is_some_and(|m| *m >= f) {
                    st.stats.dropped += 1;
                    continue;
                }
                let mode = if add { nxkit_nvn::marks::EVENT_SIGNAL_ADD } else { 0 };
                if self.signal(s.addr, |old| nxkit_nvn::marks::event_signalled(mode, old, s.value)) {
                    if add {
                        st.stats.adds += 1;
                    } else {
                        st.stats.writes += 1;
                    }
                } else {
                    st.stats.refused += 1;
                }
            }
        }
        st.cpu.retain(|_, m| *m > fence);
        self.applied.fetch_max(fence, Ordering::AcqRel);
        if crate::diag("events") {
            let s = std::mem::take(&mut st.stats);
            note(fence, s);
        }
    }

    /// Sets the counter word at the host address `word` to `f(old)` with an atomic compare-and-swap. False when the
    /// word is not in guest memory or not aligned.
    fn signal(&self, word: u64, f: impl Fn(u32) -> u32) -> bool {
        let Ok(off) = self.guest.address_map().offset_of_host(word, 4) else {
            return false;
        };
        // A game thread that writes the word between the load and the swap makes the swap fail; it is tried again.
        for _ in 0..64 {
            let Ok(old) = self.guest.load_u32(off) else {
                return false;
            };
            match self.guest.compare_exchange_u32(off, old, f(old)) {
                Ok(true) => return true,
                Ok(false) => continue,
                Err(_) => return false,
            }
        }
        false
    }

    #[cfg(test)]
    pub(super) fn stats(&self) -> Stats {
        locked(&self.state).stats
    }

    #[cfg(test)]
    pub(super) fn pending(&self) -> usize {
        locked(&self.state).frames.len()
    }
}

/// `NXKIT_DIAG=events`: one line per completion with signals (the first 100, then every 600th).
fn note(fence: u64, s: Stats) {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    if n < 100 || n.is_multiple_of(600) {
        eprintln!(
            "DIAG events fence {fence}: GPU signals written {} added {} dropped for a newer CPU signal {} \
             outside guest memory {}",
            s.writes,
            s.adds,
            s.dropped,
            s.refused
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detail::REPORT_EVENT_WRITE;

    /// Guest memory of the tests, None without a Metal device.
    struct Mem {
        space: GuestSpace,
    }

    impl Mem {
        fn new() -> Option<Self> {
            let ctx = nxkit_metal::Context::system_default()
                .inspect_err(|e| eprintln!("skipped: no usable Metal 4 device ({e})"))
                .ok()?;
            let mem = Arc::new(nxkit_metal::GuestMemory::allocate(&ctx, 0x4000).ok()?);
            Some(Self {
                space: GuestSpace::over(&mem).ok()?,
            })
        }

        fn signals(&self) -> Arc<EventSignals> {
            EventSignals::new(self.space.clone())
        }

        fn host(&self, off: u64) -> u64 {
            self.space.host_base() + off
        }

        fn get(&self, off: u64) -> u32 {
            self.space.load_u32(off).unwrap()
        }

        fn set(&self, off: u64, v: u32) {
            self.space.write(off, &v.to_le_bytes()).unwrap();
        }
    }

    fn write(addr: u64, value: u32) -> CounterReport {
        CounterReport {
            kind: REPORT_EVENT_WRITE,
            value,
            addr,
        }
    }

    fn add(addr: u64, value: u32) -> CounterReport {
        CounterReport {
            kind: REPORT_EVENT_ADD,
            value,
            addr,
        }
    }

    /// Holds the signals of `fence` for a later completion (a batch on the GPU), without a batch to hook.
    fn hold_pending(ev: &Arc<EventSignals>, fence: u64, reports: &[CounterReport]) {
        let signals: Vec<_> = reports.iter().filter(|r| r.is_event()).copied().collect();
        locked(&ev.state).frames.push_back((fence, signals));
    }

    #[test]
    fn a_signal_reaches_guest_memory_only_when_its_frame_is_complete() {
        let Some(mem) = Mem::new() else {
            return;
        };
        let ev = mem.signals();
        let w = mem.host(0x40);
        hold_pending(&ev, 1, &[write(w, 7)]);
        assert_eq!(mem.get(0x40), 0, "the GPU has not finished the frame");
        ev.complete(1);
        assert_eq!(mem.get(0x40), 7);
        assert_eq!(ev.pending(), 0);
    }

    #[test]
    fn add_adds_with_wrap_and_write_writes_in_command_order() {
        let Some(mem) = Mem::new() else {
            return;
        };
        let ev = mem.signals();
        let w = mem.host(0x80);
        mem.set(0x80, u32::MAX);
        hold_pending(&ev, 1, &[add(w, 2), add(w, 3), write(w, 10), add(w, 1)]);
        ev.complete(1);
        assert_eq!(mem.get(0x80), 11);
        let s = ev.stats();
        assert_eq!((s.adds, s.writes, s.dropped), (3, 1, 0));
    }

    #[test]
    fn the_completion_of_a_frame_writes_every_older_frame_first() {
        let Some(mem) = Mem::new() else {
            return;
        };
        let ev = mem.signals();
        let w = mem.host(0x40);
        hold_pending(&ev, 1, &[write(w, 1)]);
        hold_pending(&ev, 2, &[write(w, 2)]);
        hold_pending(&ev, 3, &[write(w, 3)]);
        ev.complete(2);
        assert_eq!(mem.get(0x40), 2);
        assert_eq!(ev.pending(), 1);
        // A late completion of an older frame changes nothing.
        ev.complete(1);
        assert_eq!(mem.get(0x40), 2);
        ev.complete(3);
        assert_eq!(mem.get(0x40), 3);
    }

    #[test]
    fn a_frame_without_a_batch_is_written_at_once_and_other_reports_are_left() {
        let Some(mem) = Mem::new() else {
            return;
        };
        let ev = mem.signals();
        let w = mem.host(0x40);
        let timestamp = CounterReport {
            kind: 0,
            value: 0,
            addr: w,
        };
        ev.hold(1, &[timestamp, write(w, 5)], None);
        assert_eq!(mem.get(0x40), 5);
        ev.hold(2, &[timestamp], None);
        assert_eq!(ev.pending(), 0);
        assert_eq!(ev.applied.load(Ordering::Acquire), 1, "a frame without signals is not followed");
    }

    #[test]
    fn a_cpu_signal_before_the_completion_is_older_than_the_gpu_write() {
        let Some(mem) = Mem::new() else {
            return;
        };
        let ev = mem.signals();
        let w = mem.host(0x40);
        hold_pending(&ev, 1, &[write(w, 9)]);
        // No wait returned for the frame: on the console the GPU reaches the signal after the CPU wrote.
        mem.set(0x40, 4);
        ev.cpu_signalled(w);
        ev.complete(1);
        assert_eq!(mem.get(0x40), 9);
    }

    #[test]
    fn a_cpu_signal_after_an_early_wait_wins_over_the_late_gpu_write_but_not_over_an_add() {
        let Some(mem) = Mem::new() else {
            return;
        };
        let ev = mem.signals();
        let (w, v) = (mem.host(0x40), mem.host(0x80));
        hold_pending(&ev, 1, &[write(w, 9), add(v, 5)]);
        hold_pending(&ev, 2, &[write(w, 11)]);
        // The wait for fence 1 returned before the GPU finished it; the CPU then resets both counters.
        ev.waited(1);
        mem.set(0x40, 0);
        ev.cpu_signalled(w);
        mem.set(0x80, 100);
        ev.cpu_signalled(v);
        ev.complete(1);
        assert_eq!(mem.get(0x40), 0, "the CPU's write is newer than the GPU's of frame 1");
        assert_eq!(mem.get(0x80), 105, "an add counts in either order");
        assert_eq!(ev.stats().dropped, 1);
        // Frame 2 is after the wait: its write is newer than the CPU's.
        ev.complete(2);
        assert_eq!(mem.get(0x40), 11);
        assert!(locked(&ev.state).cpu.is_empty(), "the marks of completed frames are dropped");
    }

    #[test]
    fn a_cpu_signal_after_an_honest_wait_takes_no_lock() {
        let Some(mem) = Mem::new() else {
            return;
        };
        let ev = mem.signals();
        let w = mem.host(0x40);
        ev.hold(1, &[write(w, 3)], None);
        ev.waited(1);
        ev.cpu_signalled(w);
        assert!(locked(&ev.state).cpu.is_empty());
    }

    #[test]
    fn a_word_outside_guest_memory_is_refused() {
        let Some(mem) = Mem::new() else {
            return;
        };
        let ev = mem.signals();
        hold_pending(&ev, 1, &[write(8, 1), write(mem.host(0x42), 1)]);
        ev.complete(1);
        assert_eq!(ev.stats().refused, 2);
        assert_eq!((mem.get(0x40), mem.get(0x44)), (0, 0));
    }
}
