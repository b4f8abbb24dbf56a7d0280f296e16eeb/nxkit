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
use std::sync::atomic::{AtomicUsize, Ordering};
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

    /// Ends every command buffer and commits them in order, in one commit. Returns at once.
    ///
    /// One commit for the whole batch: committed one by one, the GPU paused 0.2 to 1 ms at every command buffer
    /// boundary (Breath of the Wild, six command buffers a frame).
    pub fn submit(mut self) -> Result<SubmittedBatch> {
        let cpu_encode_ns = u64::try_from(self.t0.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let number = self.number;
        self.done.set_pending();
        let queue = self.ctx.queue();
        let mut held = vec![Keep::new(self.scratch.clone())];
        let mut list: Vec<NonNull<ProtocolObject<dyn MTL4CommandBuffer>>> =
            Vec::with_capacity(self.buffers.len());
        for cb in &mut self.buffers {
            cb.raw().endCommandBuffer();
            list.push(NonNull::from(&**cb.raw()));
            held.push(Keep::new(cb.take_retained()));
        }
        if list.is_empty() {
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
        let held = Mutex::new(Some(held));
        let cbs = list.len();
        let done = self.done.clone();
        let metrics = self.metrics.clone();
        let handler: FeedbackBlock =
            RcBlock::new(move |fb: NonNull<ProtocolObject<dyn MTL4CommitFeedback>>| {
                // SAFETY: Metal passes a valid feedback object that lives for the call.
                let fb = unsafe { fb.as_ref() };
                let (start, end) = (fb.GPUStartTime(), fb.GPUEndTime());
                let stats = CommandBufferStats {
                    frame: number,
                    cpu_encode_ns,
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
                metrics.record(&stats);
                if let Some(h) = DONE_HOOK.get() {
                    h(number, cbs, stats.gpu_start_s, stats.gpu_end_s);
                }
                done.add(&stats, true);
            });
        let opts = MTL4CommitOptions::new();
        opts.addFeedbackHandler(&handler);
        // SAFETY: `list` holds `list.len()` valid, ended command buffers, kept alive by `held` until the feedback.
        unsafe { queue.commit_count_options(NonNull::from(&list[0]), list.len(), &opts) };
        Ok(SubmittedBatch {
            number,
            done: self.done.clone(),
        })
    }
}
