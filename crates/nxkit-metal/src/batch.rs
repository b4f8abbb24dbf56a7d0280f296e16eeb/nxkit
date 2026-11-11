//! Frames that are encoded into several command buffers, possibly on several threads.
//!
//! A [`BatchRing`] has the same shape as the `FrameRing`: a slot per frame in flight with its own allocators and
//! scratch buffer. The difference is that a [`Batch`] holds several command buffers. A Metal 4 command allocator
//! must not be used by two command buffers that encode at the same time, so each command buffer of a batch has
//! its own allocator. The caller encodes independent passes on separate command buffers, for example on several
//! threads, and [`Batch::submit`] commits them in the order of the slice.
//!
//! Every command buffer is committed with its own call and its own feedback handler. That keeps the order of the
//! queue equal to the order of the slice and gives a GPU time per command buffer. The batch is complete when the
//! last handler ran. Presenting a drawable is not part of a batch.

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Instant;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTL4CommandAllocator, MTL4CommandBuffer, MTL4CommandQueue, MTL4CommitFeedback,
    MTL4CommitOptions, MTLDevice,
};

use crate::device::Context;
use crate::encode::CommandBuffer;
use crate::error::{MetalError, Result, create};
use crate::metrics::{CommandBufferStats, Metrics};
use crate::scratch::Scratch;
use crate::table::Keep;

/// Called when a batch completes on the GPU: `(batch number, command buffers, GPU start, GPU end)`, the times in
/// seconds of the host clock. For the diagnostics of the host; set once.
static DONE_HOOK: std::sync::OnceLock<fn(u64, usize, f64, f64)> = std::sync::OnceLock::new();

/// Sets the hook of [`DONE_HOOK`]. The first call wins.
pub fn set_done_hook(f: fn(u64, usize, f64, f64)) {
    let _ = DONE_HOOK.set(f);
}

/// Called when the first command buffer group of a batch is committed: `(batch number, host clock in ns)`, the clock of
/// `clock_gettime_nsec_np(CLOCK_UPTIME_RAW)`, which the GPU start and end times use too. For the diagnostics of the host;
/// set once.
static COMMIT_HOOK: std::sync::OnceLock<fn(u64, u64)> = std::sync::OnceLock::new();

/// Sets the hook of [`COMMIT_HOOK`]. The first call wins.
pub fn set_commit_hook(f: fn(u64, u64)) {
    let _ = COMMIT_HOOK.set(f);
}

unsafe extern "C" {
    fn clock_gettime_nsec_np(clock_id: u32) -> u64;
}

/// The host clock in ns (`CLOCK_UPTIME_RAW`).
fn uptime_ns() -> u64 {
    // SAFETY: `clock_gettime_nsec_np` has no preconditions.
    unsafe { clock_gettime_nsec_np(8) }
}

/// The number of batches the CPU can run ahead of the GPU.
pub const BATCHES_IN_FLIGHT: usize = 3;

/// The size of the first scratch buffer of each slot. A slot that needs more adds buffers that double in size up to
/// [`BATCH_SCRATCH_MAX_BYTES`]. A slot that never needs more (the game's constants are a few MB a frame) keeps 8 MiB,
/// and with the three slots of the ring that is 24 MiB, not 192 MiB.
pub const BATCH_SCRATCH_BYTES: u64 = 8 * 1024 * 1024;

/// The largest buffer that a slot adds to its scratch.
pub const BATCH_SCRATCH_MAX_BYTES: u64 = 64 * 1024 * 1024;

type Callback = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct DoneState {
    pending: bool,
    /// The last handler is running the callbacks; the batch is not yet reported as complete.
    finishing: bool,
    callbacks: Vec<Callback>,
    stats: CommandBufferStats,
}

struct Completion {
    state: Mutex<DoneState>,
    cv: Condvar,
}

impl Completion {
    fn set_pending(&self) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.pending = true;
        st.finishing = false;
    }

    fn add(&self, s: &CommandBufferStats, last: bool) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let a = &mut st.stats;
        a.frame = s.frame;
        a.cpu_encode_ns = a.cpu_encode_ns.max(s.cpu_encode_ns);
        a.gpu_ns += s.gpu_ns;
        a.gpu_start_s = if a.gpu_start_s == 0.0 {
            s.gpu_start_s
        } else {
            a.gpu_start_s.min(s.gpu_start_s)
        };
        a.gpu_end_s = a.gpu_end_s.max(s.gpu_end_s);
        a.failed |= s.failed;
        if last {
            // The callbacks come first and the waiters after them, so that a thread that waits for the batch and then
            // touches guest memory finds the pages released.
            st.finishing = true;
            let callbacks = std::mem::take(&mut st.callbacks);
            drop(st);
            for f in callbacks {
                f();
            }
            self.state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pending = false;
            self.cv.notify_all();
        }
    }

    /// Runs `f` when the batch is complete, on the thread of the Metal completion handler and before `wait` returns; at
    /// once on this thread when the batch is complete already.
    fn on_complete(&self, f: Callback) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if st.pending && !st.finishing {
            st.callbacks.push(f);
        } else {
            drop(st);
            f();
        }
    }

    fn wait(&self) -> CommandBufferStats {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        while st.pending {
            st = self.cv.wait(st).unwrap_or_else(PoisonError::into_inner);
        }
        st.stats
    }
}

struct Slot {
    allocators: Vec<Retained<ProtocolObject<dyn MTL4CommandAllocator>>>,
    done: Arc<Completion>,
    scratch: Arc<Scratch>,
}

/// The ring of batch slots.
pub struct BatchRing {
    ctx: Context,
    slots: Vec<Slot>,
    next: u64,
    metrics: Arc<Metrics>,
}

// SAFETY: the Metal objects of a ring are only used through `&mut self`, so one thread at a time touches them.
unsafe impl Send for BatchRing {}

impl BatchRing {
    /// Makes the ring with [`BATCHES_IN_FLIGHT`] slots. A batch can hold up to `max_buffers` command buffers.
    pub fn new(ctx: &Context, max_buffers: usize) -> Result<Self> {
        if max_buffers == 0 {
            return Err(MetalError::Invalid("a batch needs at least one buffer"));
        }
        let mut slots = Vec::with_capacity(BATCHES_IN_FLIGHT);
        for _ in 0..BATCHES_IN_FLIGHT {
            let mut allocators = Vec::with_capacity(max_buffers);
            for _ in 0..max_buffers {
                allocators.push(ctx.device().newCommandAllocator().ok_or_else(|| {
                    create("command allocator", "newCommandAllocator returned nil")
                })?);
            }
            slots.push(Slot {
                allocators,
                done: Arc::new(Completion {
                    state: Mutex::new(DoneState::default()),
                    cv: Condvar::new(),
                }),
                scratch: Arc::new(Scratch::growing(
                    ctx,
                    BATCH_SCRATCH_BYTES,
                    BATCH_SCRATCH_MAX_BYTES,
                )?),
            });
        }
        ctx.commit_residency();
        Ok(Self {
            ctx: ctx.clone(),
            slots,
            next: 0,
            metrics: Arc::new(Metrics::default()),
        })
    }

    /// The counters of this ring. Every command buffer adds one record.
    pub fn metrics(&self) -> Arc<Metrics> {
        self.metrics.clone()
    }

    /// Starts the next batch with `count` command buffers. It blocks while the GPU still works on the batch that
    /// used the slot.
    pub fn begin(&mut self, count: usize) -> Result<Batch> {
        let number = self.next + 1;
        let slot = &mut self.slots[(self.next % BATCHES_IN_FLIGHT as u64) as usize];
        if count == 0 || count > slot.allocators.len() {
            return Err(MetalError::Invalid(
                "the batch has no buffers or more than the ring allows",
            ));
        }
        slot.done.wait();
        self.next = number;
        let t0 = Instant::now();
        // A completion of its own for each batch: a `SubmittedBatch` that is waited for after the slot came round
        // still gives the numbers of its own batch, not those of the batch that uses the slot now.
        slot.done = Arc::new(Completion {
            state: Mutex::new(DoneState::default()),
            cv: Condvar::new(),
        });
        slot.scratch.reset();
        self.ctx.commit_residency();
        let mut buffers = Vec::with_capacity(count);
        for allocator in &slot.allocators[..count] {
            allocator.reset();
            let cb = self
                .ctx
                .device()
                .newCommandBuffer()
                .ok_or_else(|| create("command buffer", "newCommandBuffer returned nil"))?;
            cb.beginCommandBufferWithAllocator(allocator);
            buffers.push(CommandBuffer::new(cb));
        }
        Ok(Batch {
            number,
            buffers,
            scratch: slot.scratch.clone(),
            done: slot.done.clone(),
            ctx: self.ctx.clone(),
            metrics: self.metrics.clone(),
            t0,
            early: Vec::new(),
            split: None,
            ahead: None,
        })
    }
}

type FeedbackBlock =
    RcBlock<'static, fn(NonNull<ProtocolObject<dyn MTL4CommitFeedback>>), dyn Send + Sync>;

/// A batch being encoded.
pub struct Batch {
    number: u64,
    buffers: Vec<CommandBuffer>,
    scratch: Arc<Scratch>,
    done: Arc<Completion>,
    ctx: Context,
    metrics: Arc<Metrics>,
    t0: Instant,
    /// The prefixes that report early: `(command buffers in the prefix, callback)`.
    early: Vec<(usize, Callback)>,
    /// The groups the batch is committed in, once [`Batch::commit_ahead`] or the commit made them.
    split: Option<Arc<Split>>,
    ahead: Option<Arc<Ahead>>,
}

/// A command buffer that was ended, as the pointer the commit takes.
struct Ended(NonNull<ProtocolObject<dyn MTL4CommandBuffer>>);

// SAFETY: the pointer names an ended command buffer that its batch keeps alive until the commit; the queue takes it from
// any thread.
unsafe impl Send for Ended {}

/// An ended command buffer and what it keeps alive.
type Entry = (Ended, Vec<Keep>);

/// The commit groups of a batch with prefixes that report early: group `g` holds the command buffers from the end of group
/// `g - 1` to `bounds[g]`, and is committed on its own, so its feedback says when the GPU is done with it.
struct Split {
    number: u64,
    ctx: Context,
    done: Arc<Completion>,
    metrics: Arc<Metrics>,
    /// The end of each group, in command buffers; the last is the whole batch.
    bounds: Vec<usize>,
    /// Groups whose feedback has not come yet.
    remaining: AtomicUsize,
    state: Mutex<SplitState>,
}

struct SplitState {
    finished: Vec<bool>,
    /// The callbacks of the prefixes, by the group that ends the prefix.
    callbacks: Vec<(usize, Option<Callback>)>,
    start: f64,
    end: f64,
}

impl Split {
    /// Commits group `g`: `entries` are its ended command buffers with what they keep alive.
    fn commit(
        self: &Arc<Self>,
        g: usize,
        entries: Vec<Entry>,
        scratch: Arc<Scratch>,
        encode_ns: u64,
    ) {
        let mut held: Vec<Keep> = vec![Keep::new(scratch)];
        let mut list: Vec<NonNull<ProtocolObject<dyn MTL4CommandBuffer>>> =
            Vec::with_capacity(entries.len());
        for (cb, keep) in entries {
            list.push(cb.0);
            held.push(Keep::new(keep));
        }
        let held = Mutex::new(Some(held));
        let me = self.clone();
        let (number, total) = (self.number, *self.bounds.last().unwrap_or(&0));
        let handler: FeedbackBlock =
            RcBlock::new(move |fb: NonNull<ProtocolObject<dyn MTL4CommitFeedback>>| {
                // SAFETY: Metal passes a valid feedback object that lives for the call.
                let fb = unsafe { fb.as_ref() };
                let (start, end) = (fb.GPUStartTime(), fb.GPUEndTime());
                let stats = CommandBufferStats {
                    frame: number,
                    cpu_encode_ns: encode_ns,
                    gpu_ns: ((end - start).max(0.0) * 1e9) as u64,
                    gpu_start_s: start,
                    gpu_end_s: end,
                    failed: fb.error().is_some(),
                };
                if let Some(e) = fb.error() {
                    static SHOWN: AtomicUsize = AtomicUsize::new(0);
                    if SHOWN.fetch_add(1, Ordering::Relaxed) < 32 {
                        eprintln!(
                            "nxkit-metal: warning: batch {number} command buffer failed: {} {:?}",
                            e.localizedDescription(),
                            e.userInfo()
                        );
                    }
                }
                held.lock().unwrap_or_else(PoisonError::into_inner).take();
                me.metrics.record(&stats);
                // The prefixes that ended: every group up to this one is done.
                let ready: Vec<Callback> = {
                    let mut st = me.state.lock().unwrap_or_else(PoisonError::into_inner);
                    st.finished[g] = true;
                    if st.start == 0.0 || (start != 0.0 && start < st.start) {
                        st.start = start;
                    }
                    st.end = st.end.max(end);
                    let upto = st.finished.iter().take_while(|f| **f).count();
                    st.callbacks
                        .iter_mut()
                        .filter(|(group, _)| *group < upto)
                        .filter_map(|(_, f)| f.take())
                        .collect()
                };
                for f in ready {
                    f();
                }
                let last = me.remaining.fetch_sub(1, Ordering::AcqRel) == 1;
                if last && let Some(h) = DONE_HOOK.get() {
                    let (a, b) = {
                        let st = me.state.lock().unwrap_or_else(PoisonError::into_inner);
                        (st.start, st.end)
                    };
                    h(number, total, a, b);
                }
                me.done.add(&stats, last);
            });
        let opts = MTL4CommitOptions::new();
        opts.addFeedbackHandler(&handler);
        self.ctx.commit_residency();
        if g == 0
            && let Some(h) = COMMIT_HOOK.get()
        {
            h(number, uptime_ns());
        }
        // SAFETY: `list` holds ended command buffers that `held` keeps alive until the feedback.
        unsafe {
            self.ctx
                .queue()
                .commit_count_options(NonNull::from(&list[0]), list.len(), &opts)
        };
    }
}

/// The first group of a batch, committed while the rest of the batch is still being encoded: each thread that ends one of its
/// command buffers hands it in with [`finish`](Self::finish), and the one that hands in the last commits the group.
pub struct Ahead {
    split: Arc<Split>,
    scratch: Arc<Scratch>,
    t0: Instant,
    slots: Mutex<Vec<Option<Entry>>>,
    left: AtomicUsize,
    committed: AtomicBool,
}

impl Ahead {
    /// The number of command buffers in the group.
    pub fn count(&self) -> usize {
        self.slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Ends command buffer `index` of the group (the thread that encoded it calls this once it is done with it). When it is
    /// the last one of the group, the group is committed on this thread. Returns true then.
    pub fn finish(&self, index: usize, cb: &mut CommandBuffer) -> bool {
        cb.raw().endCommandBuffer();
        let entry = (
            Ended(NonNull::from(&**cb.raw())),
            vec![Keep::new(cb.take_retained())],
        );
        let full = {
            let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
            if slots.get(index).is_none_or(Option::is_some) {
                return false;
            }
            slots[index] = Some(entry);
            self.left.fetch_sub(1, Ordering::AcqRel) == 1
        };
        if !full || self.committed.swap(true, Ordering::AcqRel) {
            return false;
        }
        let entries: Vec<Entry> = self
            .slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter_mut()
            .filter_map(Option::take)
            .collect();
        let ns = u64::try_from(self.t0.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.split.commit(0, entries, self.scratch.clone(), ns);
        true
    }
}

/// A batch that is on the GPU. A clone waits for the same batch.
#[derive(Clone)]
pub struct SubmittedBatch {
    number: u64,
    done: Arc<Completion>,
}

impl SubmittedBatch {
    /// The number of the batch.
    pub fn number(&self) -> u64 {
        self.number
    }

    /// Runs `f` when the GPU finished every command buffer of the batch. It runs on the thread of the Metal completion
    /// handler, before a `wait` for the batch returns, so it must not wait for anything or take a lock that a thread
    /// holds while it waits for this batch. When the batch is complete already, `f` runs at once on this thread.
    pub fn on_complete(&self, f: impl FnOnce() + Send + 'static) {
        self.done.on_complete(Box::new(f));
    }

    /// Waits until the GPU finished every command buffer. The numbers are the sum of the GPU times, the widest
    /// GPU interval and the longest CPU encode time.
    pub fn wait(&self) -> Result<CommandBufferStats> {
        let s = self.done.wait();
        if s.failed {
            return Err(MetalError::Gpu(format!("batch {} failed", self.number)));
        }
        Ok(s)
    }
}

impl Batch {
    /// The number of the batch, from 1.
    pub fn number(&self) -> u64 {
        self.number
    }

    /// The command buffers in commit order. Hand each to one thread with `chunks_mut` or `iter_mut`.
    pub fn buffers(&mut self) -> &mut [CommandBuffer] {
        &mut self.buffers
    }

    /// The scratch buffer of the batch. It is free again when the slot comes round.
    pub fn scratch(&self) -> Arc<Scratch> {
        self.scratch.clone()
    }

    /// Runs `f` when the GPU is done with the first `count` command buffers of the batch, before the batch is complete: the
    /// batch is committed in groups then, one commit for the prefix and one for the rest. Call it before
    /// [`commit_ahead`](Self::commit_ahead) and [`submit`](Self::submit). Returns false (and drops `f`) when `count` is 0 or
    /// not smaller than the number of command buffers, or the groups are fixed already.
    ///
    /// The queue runs the commits in order, so the prefix is done when its commit is; a commit costs the GPU a short pause
    /// at the boundary (about 0.2 ms), which is the price of the early report.
    pub fn on_prefix_complete(&mut self, count: usize, f: impl FnOnce() + Send + 'static) -> bool {
        if count == 0 || count >= self.buffers.len() || self.split.is_some() {
            return false;
        }
        self.early.push((count, Box::new(f)));
        true
    }

    fn split(&mut self) -> Arc<Split> {
        if let Some(s) = &self.split {
            return s.clone();
        }
        let mut bounds: Vec<usize> = self.early.iter().map(|e| e.0).collect();
        bounds.push(self.buffers.len());
        bounds.sort_unstable();
        bounds.dedup();
        let callbacks: Vec<(usize, Option<Callback>)> = std::mem::take(&mut self.early)
            .into_iter()
            .map(|(count, f)| {
                (
                    bounds.iter().position(|b| *b == count).unwrap_or(0),
                    Some(f),
                )
            })
            .collect();
        let split = Arc::new(Split {
            number: self.number,
            ctx: self.ctx.clone(),
            done: self.done.clone(),
            metrics: self.metrics.clone(),
            remaining: AtomicUsize::new(bounds.len()),
            state: Mutex::new(SplitState {
                finished: vec![false; bounds.len()],
                callbacks,
                start: 0.0,
                end: 0.0,
            }),
            bounds,
        });
        self.split = Some(split.clone());
        split
    }

    /// For a batch with a prefix: the handle that commits the first group while the rest of the batch is encoded. Every
    /// command buffer of the group must be handed to [`Ahead::finish`] by the thread that encoded it (the first one,
    /// `buffers()[0]`, included), and nothing may be encoded into them afterwards. [`submit`](Self::submit) then commits
    /// only the other groups. None when the batch has no prefix.
    pub fn commit_ahead(&mut self) -> Option<Arc<Ahead>> {
        if let Some(a) = &self.ahead {
            return Some(a.clone());
        }
        if self.early.is_empty() && self.split.is_none() {
            return None;
        }
        let split = self.split();
        if split.bounds.len() < 2 {
            return None;
        }
        let n = split.bounds[0];
        self.done.set_pending();
        let ahead = Arc::new(Ahead {
            split,
            scratch: self.scratch.clone(),
            t0: self.t0,
            slots: Mutex::new((0..n).map(|_| None).collect()),
            left: AtomicUsize::new(n),
            committed: AtomicBool::new(false),
        });
        self.ahead = Some(ahead.clone());
        Some(ahead)
    }

    /// Streams the batch: the first `count` command buffers are committed as soon as each one is ended (by
    /// [`Ahead::finish`], on the thread that encoded it) while the other command buffers are still being encoded, so
    /// the GPU starts after that prefix and not after the whole batch. The order of the command buffers stays the order of
    /// the slice. Returns None when `count` is 0 or not smaller than the number of command buffers, or when the groups are
    /// fixed already. Every command buffer of the prefix must be handed to [`Ahead::finish`], and nothing may be encoded
    /// into them afterwards; [`submit`](Self::submit) then commits only the rest. The commit costs the GPU a short pause at
    /// the boundary (about 0.2 ms).
    pub fn stream_prefix(&mut self, count: usize) -> Option<Arc<Ahead>> {
        if !self.on_prefix_complete(count, || {}) {
            return None;
        }
        self.commit_ahead()
    }

    /// Ends every command buffer and commits them in order. Returns at once.
    ///
    /// One commit for the whole batch: committed one by one, the GPU paused 0.2 to 1 ms at every command buffer
    /// boundary (Breath of the Wild, six command buffers a frame). A batch with prefixes
    /// ([`on_prefix_complete`](Self::on_prefix_complete)) is committed in as many groups as it has prefixes plus one.
    pub fn submit(mut self) -> Result<SubmittedBatch> {
        let cpu_encode_ns = u64::try_from(self.t0.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let number = self.number;
        if self.buffers.is_empty() {
            self.done.set_pending();
            self.done.add(
                &CommandBufferStats {
                    frame: number,
                    cpu_encode_ns,
                    ..Default::default()
                },
                true,
            );
            return Ok(SubmittedBatch {
                number,
                done: self.done.clone(),
            });
        }
        let has_ahead = self.ahead.is_some();
        let split = self.split();
        if !has_ahead {
            self.done.set_pending();
        }
        let ahead = self.ahead.clone();
        let ahead_done = ahead
            .as_ref()
            .is_some_and(|a| a.committed.load(Ordering::Acquire));
        // The command buffers of the groups still to commit, as ended.
        let mut ended: Vec<Entry> = Vec::with_capacity(self.buffers.len());
        for (i, cb) in self.buffers.iter_mut().enumerate() {
            let from_ahead = ahead.as_ref().and_then(|a| {
                a.slots
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get_mut(i)
                    .and_then(Option::take)
            });
            if ahead_done && i < split.bounds[0] {
                continue;
            }
            match from_ahead {
                Some(e) => ended.push(e),
                None => {
                    cb.raw().endCommandBuffer();
                    ended.push((
                        Ended(NonNull::from(&**cb.raw())),
                        vec![Keep::new(cb.take_retained())],
                    ));
                }
            }
        }
        let mut at = if ahead_done { split.bounds[0] } else { 0 };
        let mut entries = ended.into_iter();
        for (g, &end) in split.bounds.iter().enumerate() {
            if ahead_done && g == 0 {
                continue;
            }
            let group: Vec<Entry> = entries.by_ref().take(end - at).collect();
            at = end;
            split.commit(g, group, self.scratch.clone(), cpu_encode_ns);
        }
        Ok(SubmittedBatch {
            number,
            done: self.done.clone(),
        })
    }
}
