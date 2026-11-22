//! The work-stealing executor.
//!
//! Queues:
//! - one **priority injector** (a max-heap by priority, FIFO among equals) for roots and [`Ctx::submit`].
//! - one **local deque per worker** for [`Ctx::spawn`] (continuations: pushed and popped at the back, stolen from the front).
//!
//! A worker takes, in order: its own newest local task (finish started work first, keeps memory bounded), the
//! highest-priority injected task, the oldest task of another worker (steal). Before running a task it acquires the
//! task's `cost` tokens and its expected memory from the budget, a task larger than the budget runs alone.
//!
//! A panicking task is caught, recorded in the [`Report`], and the run goes on. After [`CancelHandle::cancel`] queued
//! tasks are dropped without running and running tasks can see [`Ctx::is_cancelled`].

use crate::budget::Budget;
use std::cmp::Ordering as CmpOrdering;
use std::collections::{BinaryHeap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

type Job = Box<dyn FnOnce(&Ctx<'_>) + Send + 'static>;

/// A unit of work: a closure, its priority (higher first) and its cost in tokens.
pub struct Task {
    prio: i64,
    cost: u32,
    mem: u64,
    job: Job,
}

impl std::fmt::Debug for Task {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Task")
            .field("prio", &self.prio)
            .field("cost", &self.cost)
            .field("mem", &self.mem)
            .finish_non_exhaustive()
    }
}

impl Task {
    /// A task with priority 0 and cost 1.
    pub fn new(f: impl FnOnce(&Ctx<'_>) + Send + 'static) -> Task {
        Task {
            prio: 0,
            cost: 1,
            mem: 0,
            job: Box::new(f),
        }
    }

    /// Set the expected peak memory in bytes (from the previous build's RSS history, or a default per job kind).
    /// A task larger than the whole memory budget runs alone.
    pub fn mem(mut self, bytes: u64) -> Task {
        self.mem = bytes;
        self
    }

    /// Set the priority (critical path first, then longest expected time first).
    pub fn prio(mut self, prio: i64) -> Task {
        self.prio = prio;
        self
    }

    /// Set the cost in tokens (0 is treated as 1).
    pub fn cost(mut self, cost: u32) -> Task {
        self.cost = cost.max(1);
        self
    }
}

struct Queued {
    prio: i64,
    seq: u64,
    task: Task,
}

impl PartialEq for Queued {
    fn eq(&self, o: &Self) -> bool {
        self.prio == o.prio && self.seq == o.seq
    }
}
impl Eq for Queued {}
impl PartialOrd for Queued {
    fn partial_cmp(&self, o: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(o))
    }
}
impl Ord for Queued {
    fn cmp(&self, o: &Self) -> CmpOrdering {
        // Higher priority first, among equals, the earlier submission first.
        self.prio.cmp(&o.prio).then_with(|| o.seq.cmp(&self.seq))
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Shared {
    injector: Mutex<BinaryHeap<Queued>>,
    locals: Vec<Mutex<VecDeque<Task>>>,
    seq: AtomicU64,
    /// Tasks queued or running.
    pending: AtomicUsize,
    /// Work-available epoch, bumped on every push, waited on by idle workers.
    epoch: Mutex<u64>,
    wake: Condvar,
    free: Mutex<Free>,
    free_cv: Condvar,
    total_tokens: u32,
    /// 0 = no memory limit.
    total_mem: u64,
    cancelled: Arc<AtomicBool>,
    run: AtomicU64,
    dropped: AtomicU64,
    steals: AtomicU64,
    peak_tokens: AtomicU64,
    peak_mem: AtomicU64,
    panics: Mutex<Vec<String>>,
}

/// Resources not held by running tasks.
struct Free {
    tokens: u32,
    mem: u64,
}

/// What a running task holds.
#[derive(Clone, Copy)]
struct Held {
    tokens: u32,
    mem: u64,
}

impl Shared {
    fn notify(&self) {
        *lock(&self.epoch) += 1;
        self.wake.notify_one();
    }

    fn inject(&self, task: Task) {
        self.pending.fetch_add(1, Ordering::SeqCst);
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        lock(&self.injector).push(Queued {
            prio: task.prio,
            seq,
            task,
        });
        self.notify();
    }

    fn push_local(&self, worker: usize, task: Task) {
        self.pending.fetch_add(1, Ordering::SeqCst);
        match self.locals.get(worker) {
            Some(q) => lock(q).push_back(task),
            None => {
                self.pending.fetch_sub(1, Ordering::SeqCst);
                self.inject(task);
                return;
            }
        }
        self.notify();
    }

    fn find(&self, me: usize) -> Option<Task> {
        if let Some(t) = self.locals.get(me).and_then(|q| lock(q).pop_back()) {
            return Some(t);
        }
        if let Some(q) = lock(&self.injector).pop() {
            return Some(q.task);
        }
        let n = self.locals.len();
        for k in 1..n {
            let victim = (me + k) % n;
            if let Some(t) = lock(&self.locals[victim]).pop_front() {
                self.steals.fetch_add(1, Ordering::Relaxed);
                return Some(t);
            }
        }
        None
    }

    /// Wait until the task's tokens and memory are free, then take them. Requests larger than the budget are
    /// clamped to it, so such a task runs alone.
    fn acquire(&self, cost: u32, mem: u64) -> Held {
        let want = Held {
            tokens: cost.min(self.total_tokens).max(1),
            mem: if self.total_mem == 0 {
                0
            } else {
                mem.min(self.total_mem)
            },
        };
        let mut free = lock(&self.free);
        while free.tokens < want.tokens || free.mem < want.mem {
            free = self
                .free_cv
                .wait(free)
                .unwrap_or_else(PoisonError::into_inner);
        }
        free.tokens -= want.tokens;
        free.mem -= want.mem;
        self.peak_tokens.fetch_max(
            u64::from(self.total_tokens - free.tokens),
            Ordering::Relaxed,
        );
        self.peak_mem
            .fetch_max(self.total_mem - free.mem, Ordering::Relaxed);
        want
    }

    fn release(&self, held: Held) {
        let mut free = lock(&self.free);
        free.tokens += held.tokens;
        free.mem += held.mem;
        drop(free);
        self.free_cv.notify_all();
    }

    fn finish_one(&self) {
        if self.pending.fetch_sub(1, Ordering::SeqCst) == 1 {
            // The last task is done: wake everyone so they can exit.
            *lock(&self.epoch) += 1;
            self.wake.notify_all();
        }
    }
}

/// What a running task sees: spawn more work, check cancellation, know its worker.
pub struct Ctx<'a> {
    shared: &'a Shared,
    worker: usize,
}

impl Ctx<'_> {
    /// Queue a continuation on this worker's local deque (runs soon, here or stolen by an idle worker).
    pub fn spawn(&self, task: Task) {
        self.shared.push_local(self.worker, task);
    }

    /// Queue a task in global priority order (use for independent work whose order matters: LPT, critical path).
    pub fn submit(&self, task: Task) {
        self.shared.inject(task);
    }

    /// The worker index running this task.
    pub fn worker(&self) -> usize {
        self.worker
    }

    /// True after cancellation, long tasks should stop early.
    pub fn is_cancelled(&self) -> bool {
        self.shared.cancelled.load(Ordering::Relaxed)
    }
}

/// Cancels the executor's current and future runs.
#[derive(Clone, Debug)]
pub struct CancelHandle {
    flag: Arc<AtomicBool>,
}

impl CancelHandle {
    /// Stop: queued tasks are dropped, running ones finish (or check [`Ctx::is_cancelled`]).
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }
}

/// What a run did.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// Tasks that ran (including those that panicked).
    pub tasks_run: u64,
    /// Tasks dropped because of cancellation.
    pub cancelled: u64,
    /// Panic messages of tasks that panicked.
    pub panics: Vec<String>,
    /// Tasks taken from another worker's deque.
    pub steals: u64,
    /// Busy time per worker.
    pub busy: Vec<Duration>,
    /// Wall time of the run.
    pub wall: Duration,
    /// Most tokens in use at once.
    pub peak_tokens: u64,
    /// Most expected memory held at once (0 without a memory budget).
    pub peak_mem: u64,
}

impl Report {
    /// Busy worker time divided by wall time × workers (0 to 1).
    pub fn utilisation(&self) -> f64 {
        let workers = self.busy.len() as f64;
        let wall = self.wall.as_secs_f64();
        if workers == 0.0 || wall == 0.0 {
            return 0.0;
        }
        self.busy.iter().map(Duration::as_secs_f64).sum::<f64>() / (wall * workers)
    }
}

/// Runs task graphs on a fixed set of worker threads under a token budget.
#[derive(Debug)]
pub struct Executor {
    budget: Budget,
    cancelled: Arc<AtomicBool>,
}

impl Executor {
    /// An executor with this budget.
    pub fn new(budget: Budget) -> Executor {
        Executor {
            budget,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The budget.
    pub fn budget(&self) -> Budget {
        self.budget
    }

    /// A handle that cancels runs of this executor.
    pub fn cancel_handle(&self) -> CancelHandle {
        CancelHandle {
            flag: self.cancelled.clone(),
        }
    }

    /// Run `roots` and everything they spawn, returns when all tasks are done or dropped.
    pub fn run(&self, roots: Vec<Task>) -> Report {
        let workers = self.budget.workers.max(1);
        let shared = Shared {
            injector: Mutex::new(BinaryHeap::new()),
            locals: (0..workers).map(|_| Mutex::new(VecDeque::new())).collect(),
            seq: AtomicU64::new(0),
            pending: AtomicUsize::new(0),
            epoch: Mutex::new(0),
            wake: Condvar::new(),
            free: Mutex::new(Free {
                tokens: self.budget.tokens.max(1),
                mem: self.budget.mem_bytes,
            }),
            free_cv: Condvar::new(),
            total_tokens: self.budget.tokens.max(1),
            total_mem: self.budget.mem_bytes,
            cancelled: self.cancelled.clone(),
            run: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            steals: AtomicU64::new(0),
            peak_tokens: AtomicU64::new(0),
            peak_mem: AtomicU64::new(0),
            panics: Mutex::new(Vec::new()),
        };
        for t in roots {
            shared.inject(t);
        }
        let start = Instant::now();
        let busy: Vec<Duration> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..workers)
                .map(|w| {
                    let shared = &shared;
                    std::thread::Builder::new()
                        .name(format!("nxb-worker-{w}"))
                        .spawn_scoped(s, move || worker_loop(shared, w))
                })
                .collect();
            handles
                .into_iter()
                .map(|h| match h {
                    Ok(h) => h.join().unwrap_or_default(),
                    Err(_) => Duration::ZERO,
                })
                .collect()
        });
        // If no worker thread could start, drop what is queued rather than hang.
        let leftover = drain_all(&shared);
        Report {
            tasks_run: shared.run.load(Ordering::Relaxed),
            cancelled: shared.dropped.load(Ordering::Relaxed) + leftover,
            panics: std::mem::take(&mut *lock(&shared.panics)),
            steals: shared.steals.load(Ordering::Relaxed),
            busy,
            wall: start.elapsed(),
            peak_tokens: shared.peak_tokens.load(Ordering::Relaxed),
            peak_mem: shared.peak_mem.load(Ordering::Relaxed),
        }
    }
}

fn drain_all(shared: &Shared) -> u64 {
    let mut n = 0u64;
    n += lock(&shared.injector).drain().count() as u64;
    for q in &shared.locals {
        n += lock(q).drain(..).count() as u64;
    }
    n
}

fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic with a non-string payload".to_string()
    }
}

fn worker_loop(shared: &Shared, me: usize) -> Duration {
    let mut busy = Duration::ZERO;
    loop {
        let seen = *lock(&shared.epoch);
        if let Some(task) = shared.find(me) {
            if shared.cancelled.load(Ordering::Relaxed) {
                drop(task);
                shared.dropped.fetch_add(1, Ordering::Relaxed);
                shared.finish_one();
                continue;
            }
            let held = shared.acquire(task.cost, task.mem);
            let t0 = Instant::now();
            let ctx = Ctx { shared, worker: me };
            let job = task.job;
            if let Err(p) = catch_unwind(AssertUnwindSafe(|| job(&ctx))) {
                lock(&shared.panics).push(panic_text(p.as_ref()));
            }
            busy += t0.elapsed();
            shared.release(held);
            shared.run.fetch_add(1, Ordering::Relaxed);
            shared.finish_one();
            continue;
        }
        if shared.pending.load(Ordering::SeqCst) == 0 {
            return busy;
        }
        let g = lock(&shared.epoch);
        if *g == seen && shared.pending.load(Ordering::SeqCst) > 0 {
            // The timeout is a safety net only, pushes and the last finish notify.
            let _ = shared.wake.wait_timeout(g, Duration::from_millis(50));
        }
    }
}
