//! `nvnQueueFlush`: the work that a queue was given reaches the GPU without a further call.
//!
//! On the console a flushed submit runs on the GPU by itself. A game may then poll the memory of a counter report
//! (`nvnCommandBufferReportCounter`) or of an event that the work writes, with no `nvnSyncWait` and no present. Here
//! the work of the open frame reaches the GPU only at a flush of the frame: a present, a CPU wait, a release. A flush
//! for every `nvnQueueFlush` would split the frames (Unreal Engine's RHI thread calls it several times a frame), so the
//! call only starts a deadline instead:
//!
//! - Every submit that goes into the frame counts (`submitted`), and every flush of the frame notes the count it took
//!   along (`sent`). Both change under the front lock.
//! - `nvnQueueFlush` asks that the submits counted so far reach the GPU. When [`DELAY`] has passed and a flush has not
//!   taken them along yet, a watcher thread flushes the frame ([`FlushReason::Queue`]). The decision looks at the
//!   submits themselves, not at presents: a present before the deadline that flushed older work only does not end it.
//!
//! In a frame that presents in time the request costs a lock and a notify. Unreal Engine 4.18's
//! `RHIGetRenderQueryResult` waits for the report of an occlusion query like that. Octopath Traveler's render thread
//! waited in it for minutes on the way out of the Atlasdam academy, while the game thread held the frame for a render
//! fence: no present and no CPU wait came that would have flushed the query's work.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, Once, PoisonError};
use std::time::{Duration, Instant};

use nxkit_nvn::front::FlushReason;

/// How long a request waits for a flush that comes anyway: longer than a frame at 30 fps, so that a present ends it.
pub(crate) const DELAY: Duration = Duration::from_millis(50);

/// A pending request: the submits it asks for and when it came.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Due {
    pub(crate) since: Instant,
    pub(crate) submits: u64,
}

/// What the watcher does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// A flush took the submits along: the request is done.
    Done,
    /// Look again after this long.
    Wait(Duration),
    /// Flush the frame now.
    Flush,
}

/// What to do with `due` at `now`, when the flushes took `sent` submits along.
pub(crate) fn step(due: Due, now: Instant, sent: u64, delay: Duration) -> Step {
    if sent >= due.submits {
        return Step::Done;
    }
    match delay.checked_sub(now.saturating_duration_since(due.since)) {
        Some(left) if !left.is_zero() => Step::Wait(left),
        _ => Step::Flush,
    }
}

/// The new pending request when `nvnQueueFlush` asks for `submits` at `now`. A pending request that a flush has not
/// served keeps its deadline and grows to the newer submits. A served one starts again.
pub(crate) fn merge(old: Option<Due>, submits: u64, sent: u64, now: Instant) -> Option<Due> {
    if submits <= sent {
        return old.filter(|d| d.submits > sent);
    }
    Some(match old {
        Some(d) if d.submits > sent => Due {
            since: d.since,
            submits: d.submits.max(submits),
        },
        _ => Due {
            since: now,
            submits,
        },
    })
}

/// The counters and the pending request of the context.
#[derive(Debug)]
pub(crate) struct QueueFlush {
    submitted: AtomicU64,
    sent: AtomicU64,
    due: Mutex<Option<Due>>,
    cv: Condvar,
    watcher: Once,
}

// `Once` implements `Default` only from Rust 1.100 on, and the workspace builds with 1.99.
impl Default for QueueFlush {
    fn default() -> Self {
        Self {
            submitted: AtomicU64::new(0),
            sent: AtomicU64::new(0),
            due: Mutex::new(None),
            cv: Condvar::new(),
            watcher: Once::new(),
        }
    }
}

impl QueueFlush {
    /// A submit went into the frame. The caller holds the front lock.
    pub(crate) fn submitted(&self) {
        self.submitted.fetch_add(1, Ordering::AcqRel);
    }

    /// A flush takes every submit of the frame along. The caller holds the front lock.
    pub(crate) fn sent(&self) {
        self.sent
            .store(self.submitted.load(Ordering::Acquire), Ordering::Release);
    }

    /// The submits in the frame that no flush took along yet.
    #[cfg(test)]
    pub(crate) fn unsent(&self) -> u64 {
        self.submitted.load(Ordering::Acquire) - self.sent.load(Ordering::Acquire)
    }

    /// `nvnQueueFlush`: the submits so far are to reach the GPU. `flush` is what the watcher calls when the deadline
    /// passes. The first request starts the watcher.
    pub(crate) fn request(&'static self, flush: fn(FlushReason)) {
        let submits = self.submitted.load(Ordering::Acquire);
        let sent = self.sent.load(Ordering::Acquire);
        let mut due = self.due.lock().unwrap_or_else(PoisonError::into_inner);
        let new = merge(*due, submits, sent, Instant::now());
        if new != *due {
            *due = new;
            self.cv.notify_one();
        }
        drop(due);
        self.watcher.call_once(|| {
            let spawned = std::thread::Builder::new()
                .name("nxkit-qflush".into())
                .spawn(move || self.watch(flush));
            if let Err(e) = spawned {
                eprintln!(
                    "nxkit-rt: the queue flush thread did not start, nvnQueueFlush waits for the next present: {e}"
                );
            }
        });
    }

    fn watch(&self, flush: fn(FlushReason)) {
        let mut due = self.due.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            let Some(d) = *due else {
                due = self.cv.wait(due).unwrap_or_else(PoisonError::into_inner);
                continue;
            };
            match step(d, Instant::now(), self.sent.load(Ordering::Acquire), DELAY) {
                Step::Done => *due = None,
                Step::Wait(left) => {
                    due = self
                        .cv
                        .wait_timeout(due, left)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
                Step::Flush => {
                    *due = None;
                    drop(due);
                    flush(FlushReason::Queue);
                    due = self.due.lock().unwrap_or_else(PoisonError::into_inner);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn due(since: Instant, submits: u64) -> Due {
        Due { since, submits }
    }

    #[test]
    fn a_flush_that_took_the_submits_along_ends_the_request() {
        let t0 = Instant::now();
        assert_eq!(step(due(t0, 7), t0, 7, DELAY), Step::Done);
        assert_eq!(step(due(t0, 7), t0 + DELAY * 3, 9, DELAY), Step::Done);
    }

    #[test]
    fn a_request_waits_out_its_delay_then_flushes() {
        let t0 = Instant::now();
        let half = DELAY / 2;
        assert_eq!(
            step(due(t0, 7), t0 + half, 6, DELAY),
            Step::Wait(DELAY - half)
        );
        assert_eq!(step(due(t0, 7), t0 + DELAY, 6, DELAY), Step::Flush);
        assert_eq!(step(due(t0, 7), t0 + DELAY * 10, 6, DELAY), Step::Flush);
    }

    #[test]
    fn a_flush_of_older_work_only_does_not_end_the_request() {
        // A present flushed submits 1..=5, and the request asks for 7.
        let t0 = Instant::now();
        assert_eq!(step(due(t0, 7), t0 + DELAY, 5, DELAY), Step::Flush);
    }

    #[test]
    fn nothing_unsent_asks_for_nothing() {
        let t0 = Instant::now();
        assert_eq!(merge(None, 4, 4, t0), None);
        assert_eq!(merge(Some(due(t0, 3)), 4, 4, t0), None);
    }

    #[test]
    fn a_pending_request_keeps_its_deadline_and_grows() {
        let t0 = Instant::now();
        let t1 = t0 + DELAY / 2;
        assert_eq!(merge(None, 5, 4, t0), Some(due(t0, 5)));
        assert_eq!(merge(Some(due(t0, 5)), 8, 4, t1), Some(due(t0, 8)));
        assert_eq!(merge(Some(due(t0, 5)), 5, 5, t1), None);
    }

    #[test]
    fn a_served_request_starts_again() {
        let t0 = Instant::now();
        let t1 = t0 + DELAY * 2;
        assert_eq!(merge(Some(due(t0, 5)), 9, 6, t1), Some(due(t1, 9)));
    }
}
