//! Timed jobs that run on a guest thread, asked for from any thread.
//!
//! A service can ask for SDK work, such as signalling a `SystemEventType`, from a host thread. The video decoder's
//! worker does. A guest thread is started only from a guest thread, so one guest thread runs all these jobs in the
//! order of their time. The first request from a guest thread starts it. A request from a host thread before then
//! waits in the queue.

use std::collections::BTreeMap;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

/// A job of the queue.
pub(crate) type Job = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct State {
    /// The jobs by their time, and by the order of the requests for the same time.
    queue: BTreeMap<(Instant, u64), Job>,
    next: u64,
    started: bool,
}

/// The queue and the state of its runner.
#[derive(Default)]
pub(crate) struct Jobs {
    state: Mutex<State>,
    wake: Condvar,
}

impl Jobs {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Adds `job` to run at `at`. True when the caller must start the runner: the first request from a guest thread.
    pub(crate) fn push(&self, at: Instant, job: Job, on_guest_thread: bool) -> bool {
        let mut st = self.lock();
        let seq = st.next;
        st.next += 1;
        st.queue.insert((at, seq), job);
        let start = on_guest_thread && !st.started;
        st.started |= start;
        drop(st);
        self.wake.notify_one();
        start
    }

    /// The first job whose time has come at `now`, taken out of the queue.
    fn take_due(st: &mut State, now: Instant) -> Option<Job> {
        let (&key, _) = st.queue.first_key_value()?;
        (key.0 <= now).then(|| st.queue.remove(&key)).flatten()
    }

    /// Waits for the next job whose time has come and gives it.
    pub(crate) fn next(&self) -> Job {
        let mut st = self.lock();
        loop {
            if let Some(job) = Self::take_due(&mut st, Instant::now()) {
                return job;
            }
            st = match st.queue.first_key_value().map(|(k, _)| k.0) {
                Some(at) => {
                    let wait = at.saturating_duration_since(Instant::now());
                    self.wake
                        .wait_timeout(st, wait)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => self.wake.wait(st).unwrap_or_else(PoisonError::into_inner),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    fn note(log: &Arc<Mutex<Vec<u32>>>, n: u32) -> Job {
        let log = Arc::clone(log);
        Box::new(move || log.lock().unwrap().push(n))
    }

    #[test]
    fn the_first_request_from_a_guest_thread_starts_the_runner() {
        let jobs = Jobs::default();
        let now = Instant::now();
        assert!(!jobs.push(now, Box::new(|| {}), false));
        assert!(jobs.push(now, Box::new(|| {}), true));
        assert!(!jobs.push(now, Box::new(|| {}), true));
    }

    #[test]
    fn jobs_run_in_the_order_of_their_time_and_then_of_their_request() {
        let jobs = Jobs::default();
        let log = Arc::new(Mutex::new(Vec::new()));
        let now = Instant::now();
        jobs.push(now + Duration::from_millis(20), note(&log, 3), false);
        jobs.push(now, note(&log, 1), false);
        jobs.push(now, note(&log, 2), false);
        for _ in 0..3 {
            jobs.next()();
        }
        assert_eq!(*log.lock().unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn a_job_from_another_thread_wakes_the_runner() {
        let jobs = Arc::new(Jobs::default());
        let ran = Arc::new(AtomicU32::new(0));
        let runner = {
            let (jobs, ran) = (Arc::clone(&jobs), Arc::clone(&ran));
            std::thread::spawn(move || {
                jobs.next()();
                ran.fetch_add(1, Ordering::SeqCst);
            })
        };
        std::thread::sleep(Duration::from_millis(10));
        jobs.push(Instant::now(), Box::new(|| {}), false);
        runner.join().unwrap();
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }
}
