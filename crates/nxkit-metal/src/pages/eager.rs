//! The eager copy of pages from the mirror at the completion of a frame, on its own thread.

use super::*;

/// The blocks a frame may write, by run and page.
pub(super) type FrameMasks = (u64, Vec<Vec<Mask>>);

/// A completed frame and the page runs it protected.
pub(super) type FrameRuns = (u64, Vec<(u32, u32)>);

/// The completed frames whose pages the eager thread looks at, oldest first.
struct EagerQueue {
    jobs: Mutex<VecDeque<FrameRuns>>,
    wake: std::sync::Condvar,
}

static EAGER: EagerQueue = EagerQueue {
    jobs: Mutex::new(VecDeque::new()),
    wake: std::sync::Condvar::new(),
};
static EAGER_THREAD: Once = Once::new();
/// True while the eager thread holds a job (set under the queue lock when it takes one).
static EAGER_BUSY: AtomicBool = AtomicBool::new(false);

/// Drops the queued jobs and waits for the one in progress: a new `attach` replaces the tables and the host may unmap the
/// guest range the job writes through the alias.
pub(super) fn eager_quiesce() {
    EAGER
        .jobs
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clear();
    while EAGER_BUSY.load(Ordering::Acquire) {
        std::thread::yield_now();
    }
}
/// The most completed frames that wait for the eager thread; older ones are dropped (their pages copy on a fault).
const EAGER_QUEUE_MAX: usize = 16;

pub(super) fn eager_submit(jobs: Vec<FrameRuns>) {
    EAGER_THREAD.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("nxkit-pages-copyback".into())
            .spawn(|| {
                loop {
                    let job = {
                        let mut q = EAGER.jobs.lock().unwrap_or_else(PoisonError::into_inner);
                        while q.is_empty() {
                            q = EAGER.wake.wait(q).unwrap_or_else(PoisonError::into_inner);
                        }
                        let job = q.pop_front();
                        if job.is_some() {
                            EAGER_BUSY.store(true, Ordering::Release);
                        }
                        job
                    };
                    if let Some((f, runs)) = job {
                        eager_copy(f, &runs);
                        EAGER_BUSY.store(false, Ordering::Release);
                    }
                }
            });
    });
    let mut q = EAGER.jobs.lock().unwrap_or_else(PoisonError::into_inner);
    q.extend(jobs);
    while q.len() > EAGER_QUEUE_MAX {
        q.pop_front();
    }
    drop(q);
    EAGER.wake.notify_one();
}

/// The eager copy of the completed frame `frame`: the pages of `runs` that are still tagged with it, that a game thread
/// faulted on for a copy in the last epochs and that nobody touched since are copied from the mirror through the alias,
/// opened for reading and set clean, in the background, so that the game thread does not take the fault. It holds the
/// control mutex per run, so a flush never sees a page in the middle of it.
fn eager_copy(frame: u64, runs: &[(u32, u32)]) {
    let alias = SHARED.alias.load(Ordering::Acquire);
    let mirror = SHARED.mirror.load(Ordering::Acquire);
    if alias == 0 || mirror == 0 || !SHARED.eager_on.load(Ordering::Acquire) {
        return;
    }
    let t0 = now_ns();
    let epoch = SHARED.epoch.load(Ordering::Relaxed);
    let (mut pages, mut calls) = (0u64, 0u64);
    for &(first, end) in runs {
        let _c = ctrl();
        let mut run: Option<(u32, u32)> = None;
        let finish = |r: (u32, u32), calls: &mut u64| {
            protect_pages(r.0, r.1, PROT_READ, false);
            *calls += 1;
            for pg in r.0..r.1 {
                if let Some(w) = word(pg as usize) {
                    w.store(CLEAN, Ordering::Release);
                }
            }
            wake_all();
        };
        for pg in first..end {
            let claimed = (|| {
                let i = info(pg as usize)?;
                let e = i.eager.load(Ordering::Relaxed);
                if e == 0 || epoch > e {
                    return None;
                }
                let w = word(pg as usize)?;
                let cur = w.load(Ordering::Acquire);
                if cur & STATE_MASK != GPU || u64::from(cur >> FRAME_SHIFT) != frame & FRAME_MASK {
                    return None;
                }
                w.compare_exchange(cur, pack(SYNC, frame), Ordering::AcqRel, Ordering::Acquire)
                    .ok()?;
                if !late_current(i, frame as u32) {
                    copy_stale(pg as usize, mirror, alias, false, frame as u32);
                }
                for a in &i.stale {
                    a.store(0, Ordering::Release);
                }
                i.snap.store((frame as u32).max(1), Ordering::Release);
                Some(())
            })()
            .is_some();
            match (claimed, run) {
                (true, None) => run = Some((pg, pg + 1)),
                (true, Some((s, _))) => run = Some((s, pg + 1)),
                (false, Some(r)) => {
                    finish(r, &mut calls);
                    run = None;
                }
                (false, None) => {}
            }
            pages += u64::from(claimed);
        }
        if let Some(r) = run {
            finish(r, &mut calls);
        }
    }
    SHARED.eager_pages.fetch_add(pages, Ordering::Relaxed);
    SHARED.eager_runs.fetch_add(calls, Ordering::Relaxed);
    SHARED
        .eager_ns
        .fetch_add(now_ns().saturating_sub(t0), Ordering::Relaxed);
}
