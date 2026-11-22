//! The reports of a frame land when its batch completes, also when no CPU wait follows.
//!
//! On the console the GPU writes a counter or timestamp report when it runs the command. A game may poll the report's
//! memory directly, with no `nvnSyncWait` and no present in between. Here the reports are written by
//! [`GpuBackend::write_counters`], which the CPU waits call. So a frame that holds reports also asks its batch to tell
//! the lander when it is complete, and the lander thread writes the reports of the finished frames up to it.
//!
//! The completion callback of a batch must not wait for a batch (the batch is not reported complete until its callbacks
//! ran), so the callback only records the fence and wakes the lander. The lander waits for the batches up to that
//! fence without taking them from `inflight`, so a later CPU wait still finds them. Both go through
//! `write_counters`, whose clock lock orders the writes. The first of the two to come takes a report.
//!
//! Unreal Engine 4.18's `RHIGetRenderQueryResult` polls the timestamp of a report like that. In Octopath Traveler the
//! render side waited in it after a traveler was confirmed, while the game thread held the frame for a render fence
//! behind the same work: no wait and no present came for about 50 s.

use std::sync::{Condvar, Mutex, PoisonError, Weak};

use super::GpuBackend;

/// The newest fence whose batch completed and has reports, and whether the lander thread runs.
#[derive(Default)]
pub(super) struct Lander {
    st: Mutex<State>,
    cv: Condvar,
}

#[derive(Default)]
struct State {
    /// The newest completed fence with reports.
    due: u64,
    /// The fence up to which the lander wrote.
    landed: u64,
    started: bool,
}

/// What the lander does next: nothing new, or write the reports up to a fence.
pub(super) fn next(due: u64, landed: u64) -> Option<u64> {
    (due > landed).then_some(due)
}

impl Lander {
    /// The batch of `fence` completed: wakes the lander. Starts it the first time, with `backend` to write through.
    pub(super) fn completed(&self, fence: u64, backend: &Weak<GpuBackend>) {
        let mut st = self.st.lock().unwrap_or_else(PoisonError::into_inner);
        st.due = st.due.max(fence);
        if !st.started {
            st.started = true;
            let weak = backend.clone();
            let spawned = std::thread::Builder::new()
                .name("nxkit-reports".into())
                .spawn(move || run(&weak));
            if let Err(e) = spawned {
                st.started = false;
                eprintln!(
                    "nxkit-gpu: the report thread did not start, reports land at CPU waits only: {e}"
                );
            }
        }
        self.cv.notify_one();
    }

    /// Waits until a fence is due. None when the backend is gone.
    fn wait_due(&self, backend: &Weak<GpuBackend>) -> Option<u64> {
        let mut st = self.st.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(f) = next(st.due, st.landed) {
                st.landed = f;
                return Some(f);
            }
            if backend.strong_count() == 0 {
                return None;
            }
            st = self
                .cv
                .wait_timeout(st, std::time::Duration::from_secs(1))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// The lander thread. It ends with the backend.
fn run(backend: &Weak<GpuBackend>) {
    loop {
        let Some(b) = backend.upgrade() else { return };
        let lander = b.lander.clone();
        drop(b);
        let Some(upto) = lander.wait_due(backend) else {
            return;
        };
        let Some(b) = backend.upgrade() else { return };
        b.land_upto(upto);
    }
}

impl GpuBackend {
    /// After the encode of `fence`: when the frame holds reports, its batch wakes the lander when it is complete.
    pub(super) fn land_when_done(&self, fence: u64, has_reports: bool) {
        if !has_reports {
            return;
        }
        let batch = super::locked(&self.inflight)
            .iter()
            .rev()
            .find(|(f, _)| *f == fence)
            .map(|(_, b)| b.clone());
        let lander = self.lander.clone();
        let me = self.this.clone();
        let done = move || lander.completed(fence, &me);
        match batch {
            Some(b) => b.on_complete(done),
            None => done(),
        }
    }

    /// Writes the reports of the frames up to `upto`, whose batches completed, without taking the batches.
    fn land_upto(&self, upto: u64) {
        let batches: Vec<_> = super::locked(&self.inflight)
            .iter()
            .filter(|(f, _)| *f <= upto)
            .map(|(f, b)| (*f, b.clone()))
            .collect();
        let finished: Vec<_> = batches
            .iter()
            .filter_map(|(f, b)| b.wait().ok().map(|s| (*f, s)))
            .collect();
        self.write_counters(upto, &finished);
    }
}

#[cfg(test)]
mod tests {
    use super::next;

    #[test]
    fn the_lander_writes_each_completed_fence_once() {
        assert_eq!(next(0, 0), None);
        assert_eq!(next(5, 0), Some(5));
        assert_eq!(next(5, 5), None);
        assert_eq!(next(7, 5), Some(7));
    }
}
