//! The flushes of a frame by reason, and how long each took to reach the GPU (`NXKIT_DIAG=frame` and `gateseq`).
//!
//! Per reason (release, present, CPU wait) it counts the flushes and, of the flushes whose batch completed, adds
//! - the time from the start of the flush to the first commit of its batch (for a release flush from the CPU event
//!   signal that released the submits, so the decode of the released submits is in it), and
//! - the time from that commit to the start of the batch on the GPU.
//!
//! The first comes from the commit hook of the metal crate, the second from the GPU start time of the done hook. Both use
//! the clock of `clock_gettime_nsec_np(CLOCK_UPTIME_RAW)`. [`take`] gives the sums since its last call; the `frame`
//! line prints them, and the `BATCH` line of `gateseq` carries the two times of its batch.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

use nxkit_nvn::front::FlushReason;

/// The reasons, in the order of the counters.
pub(crate) const NAMES: [&str; 3] = ["release", "present", "cpuwait"];

fn index(reason: FlushReason) -> usize {
    match reason {
        FlushReason::Release => 0,
        FlushReason::Present => 1,
        FlushReason::CpuWait => 2,
    }
}

unsafe extern "C" {
    fn clock_gettime_nsec_np(clock_id: u32) -> u64;
}

/// The host clock in ns: `CLOCK_UPTIME_RAW`, the clock of the GPU times of Metal.
pub(crate) fn now_ns() -> u64 {
    // SAFETY: `clock_gettime_nsec_np` has no preconditions.
    unsafe { clock_gettime_nsec_np(8) }
}

/// Whether a category that uses the counters is on. The first call also sets the hooks for the batches of the GPU.
pub(crate) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = super::frametime::on() || super::gateseq::on();
        if on {
            nxkit_nvn::diag::set_batch_hooks(nxkit_nvn::diag::BatchHooks {
                kept,
                done,
                committed,
            });
        }
        on
    })
}

/// The counters of one reason since the last [`take`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Sums {
    /// Flushes.
    pub(crate) flushes: u64,
    /// Flushes whose batch reached the GPU, with the time from the start of the flush to the first commit and from there
    /// to the GPU start, in ns.
    pub(crate) timed: u64,
    pub(crate) to_commit_ns: u64,
    pub(crate) to_start_ns: u64,
}

impl Sums {
    /// `flushes N, to first commit X ms, to GPU start Y ms` (the times are means).
    pub(crate) fn describe(&self) -> String {
        if self.timed == 0 {
            return format!("{}", self.flushes);
        }
        let mean = |ns: u64| ns as f64 / self.timed as f64 / 1e6;
        format!(
            "{} (first commit +{:.2} ms, GPU start +{:.2} ms after it)",
            self.flushes,
            mean(self.to_commit_ns),
            mean(self.to_start_ns)
        )
    }
}

static FLUSHES: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
static TIMED: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
static TO_COMMIT: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
static TO_START: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
/// The start of the release that the next release flush belongs to.
static RELEASE_AT: AtomicU64 = AtomicU64::new(0);

/// The counters since the last call, by reason (see [`NAMES`]).
pub(crate) fn take() -> [Sums; 3] {
    std::array::from_fn(|i| Sums {
        flushes: FLUSHES[i].swap(0, Ordering::Relaxed),
        timed: TIMED[i].swap(0, Ordering::Relaxed),
        to_commit_ns: TO_COMMIT[i].swap(0, Ordering::Relaxed),
        to_start_ns: TO_START[i].swap(0, Ordering::Relaxed),
    })
}

/// A CPU event signal begins to release held submits.
pub(crate) fn release_begins() {
    if on() {
        RELEASE_AT.store(now_ns(), Ordering::Relaxed);
    }
}

/// A flush of `fence` begins for `reason`.
pub(crate) fn flush(fence: u64, reason: FlushReason) {
    if !on() {
        return;
    }
    let i = index(reason);
    FLUSHES[i].fetch_add(1, Ordering::Relaxed);
    let begin = match reason {
        FlushReason::Release => RELEASE_AT.load(Ordering::Relaxed),
        _ => now_ns(),
    };
    track(|t| {
        if t.fences.len() > 64 {
            t.fences.clear();
        }
        t.fences.insert(fence, (i, begin));
    });
}

#[derive(Default)]
struct Track {
    /// Flushes whose batch was not kept yet: fence to (reason, begin).
    fences: HashMap<u64, (usize, u64)>,
    /// First commits whose batch was not kept yet: batch number to time.
    commits: HashMap<u64, u64>,
    /// Batches kept: batch number to (reason, begin, first commit), until they complete.
    batches: HashMap<u64, (usize, u64, u64)>,
}

static TRACK: Mutex<Option<Track>> = Mutex::new(None);

fn track<R>(f: impl FnOnce(&mut Track) -> R) -> R {
    let mut g = TRACK.lock().unwrap_or_else(PoisonError::into_inner);
    f(g.get_or_insert_with(Track::default))
}

fn committed(number: u64, ns: u64) {
    track(|t| {
        if t.commits.len() > 64 {
            t.commits.clear();
        }
        t.commits.insert(number, ns);
    });
}

fn kept(fence: u64, number: u64) {
    track(|t| {
        if let (Some((i, begin)), Some(commit)) = (t.fences.remove(&fence), t.commits.remove(&number)) {
            if t.batches.len() > 64 {
                t.batches.clear();
            }
            t.batches.insert(number, (i, begin, commit));
            TO_COMMIT[i].fetch_add(commit.saturating_sub(begin), Ordering::Relaxed);
        }
    });
    super::gateseq::batch_kept(fence, number);
}

fn done(number: u64, cbs: usize, start_s: f64, end_s: f64) {
    let info = track(|t| t.batches.remove(&number)).map(|(i, begin, commit)| {
        let start = (start_s * 1e9) as u64;
        let to_start = start.saturating_sub(commit);
        TIMED[i].fetch_add(1, Ordering::Relaxed);
        TO_START[i].fetch_add(to_start, Ordering::Relaxed);
        format!(
            "{} flush: first commit +{:.2} ms after its start, GPU start +{:.2} ms after the commit",
            NAMES[i],
            commit.saturating_sub(begin) as f64 / 1e6,
            to_start as f64 / 1e6
        )
    });
    super::gateseq::batch_done(number, cbs, start_s, end_s, info);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sums_describe_counts_and_mean_times() {
        let s = Sums {
            flushes: 3,
            timed: 2,
            to_commit_ns: 4_000_000,
            to_start_ns: 1_000_000,
        };
        assert_eq!(
            s.describe(),
            "3 (first commit +2.00 ms, GPU start +0.50 ms after it)"
        );
        assert_eq!(Sums { flushes: 1, ..Sums::default() }.describe(), "1");
    }

    #[test]
    fn reasons_have_their_own_counter() {
        assert_eq!(NAMES[index(FlushReason::Release)], "release");
        assert_eq!(NAMES[index(FlushReason::Present)], "present");
        assert_eq!(NAMES[index(FlushReason::CpuWait)], "cpuwait");
    }
}
