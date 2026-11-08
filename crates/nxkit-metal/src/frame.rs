//! Frames in flight. Each slot has its own command allocator and scratch buffer.
//!
//! `FrameRing::begin` waits until the GPU is done with the slot's last frame, resets the allocator and starts a
//! command buffer. `Frame::submit` ends it, commits it and records the CPU and GPU time. A frame presents its
//! drawable at once (`Frame::present`) or at the target time of a display tick (`Frame::present_at`).

use std::any::Any;
use std::ptr::NonNull;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTL4CommandAllocator, MTL4CommandBuffer, MTL4CommandQueue, MTL4CommitFeedback,
    MTL4CommitOptions, MTLDevice, MTLDrawable,
};

use crate::device::Context;
use crate::encode::CommandBuffer;
use crate::error::{MetalError, Result, create};
use crate::metrics::{CommandBufferStats, FrameHooks, Metrics};
use crate::scratch::Scratch;
use crate::stats::PacingStats;
use crate::table::Keep;

/// The number of frames the CPU can run ahead of the GPU. It is two (R19): a third frame only adds latency.
pub const FRAMES_IN_FLIGHT: usize = 2;

/// The size of the scratch buffer of each slot.
pub const SCRATCH_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Default)]
struct DoneState {
    pending: bool,
    last: Option<CommandBufferStats>,
}

/// Tells a waiter that the GPU finished a submission.
struct Completion {
    state: Mutex<DoneState>,
    cv: Condvar,
}

impl Completion {
    fn set_pending(&self) {
        if let Ok(mut s) = self.state.lock() {
            s.pending = true;
        }
    }

    fn finish(&self, stats: CommandBufferStats) {
        if let Ok(mut s) = self.state.lock() {
            s.pending = false;
            s.last = Some(stats);
        }
        self.cv.notify_all();
    }

    fn wait(&self) -> Result<CommandBufferStats> {
        let mut s = self
            .state
            .lock()
            .map_err(|_| MetalError::Invalid("completion lock poisoned"))?;
        while s.pending {
            s = self
                .cv
                .wait(s)
                .map_err(|_| MetalError::Invalid("completion lock poisoned"))?;
        }
        Ok(s.last.unwrap_or_default())
    }
}

struct Slot {
    allocator: Retained<ProtocolObject<dyn MTL4CommandAllocator>>,
    done: Arc<Completion>,
    scratch: Arc<Scratch>,
}

/// The ring of frame slots.
pub struct FrameRing {
    ctx: Context,
    slots: Vec<Slot>,
    next: u64,
    metrics: Arc<Metrics>,
    hooks: Option<Arc<dyn FrameHooks>>,
    pacing: Option<Arc<PacingStats>>,
}

// SAFETY: the Metal objects of a ring are only used through `&mut self`, so one thread at a time touches them. Metal
// allows an allocator to move between threads when its use is serial.
unsafe impl Send for FrameRing {}

impl FrameRing {
    /// Makes the ring with [`FRAMES_IN_FLIGHT`] slots.
    pub fn new(ctx: &Context) -> Result<Self> {
        let mut slots = Vec::with_capacity(FRAMES_IN_FLIGHT);
        for _ in 0..FRAMES_IN_FLIGHT {
            let allocator = ctx
                .device()
                .newCommandAllocator()
                .ok_or_else(|| create("command allocator", "newCommandAllocator returned nil"))?;
            slots.push(Slot {
                allocator,
                done: Arc::new(Completion {
                    state: Mutex::new(DoneState::default()),
                    cv: Condvar::new(),
                }),
                scratch: Arc::new(Scratch::new(ctx, SCRATCH_BYTES)?),
            });
        }
        ctx.commit_residency();
        Ok(Self {
            ctx: ctx.clone(),
            slots,
            next: 0,
            metrics: Arc::new(Metrics::default()),
            hooks: None,
            pacing: None,
        })
    }

    /// Sets the pacing counters. Each presented drawable then reports when the display showed it.
    pub fn set_pacing_stats(&mut self, stats: Arc<PacingStats>) {
        self.pacing = Some(stats);
    }

    /// Sets the pacing hooks.
    pub fn set_hooks(&mut self, hooks: Arc<dyn FrameHooks>) {
        self.hooks = Some(hooks);
    }

    /// The counters of this ring.
    pub fn metrics(&self) -> Arc<Metrics> {
        self.metrics.clone()
    }

    /// Starts the next frame. It blocks while the GPU still works on the frame that used this slot.
    pub fn begin(&mut self) -> Result<Frame> {
        let number = self.next + 1;
        let slot = &self.slots[(self.next % FRAMES_IN_FLIGHT as u64) as usize];
        slot.done.wait()?;
        self.next = number;
        let t0 = Instant::now();
        slot.allocator.reset();
        slot.scratch.reset();
        self.ctx.commit_residency();
        let cb = self
            .ctx
            .device()
            .newCommandBuffer()
            .ok_or_else(|| create("command buffer", "newCommandBuffer returned nil"))?;
        cb.beginCommandBufferWithAllocator(&slot.allocator);
        if let Some(h) = &self.hooks {
            h.frame_begin(number);
        }
        Ok(Frame {
            number,
            cb: CommandBuffer::new(cb),
            scratch: slot.scratch.clone(),
            done: slot.done.clone(),
            ctx: self.ctx.clone(),
            metrics: self.metrics.clone(),
            hooks: self.hooks.clone(),
            pacing: self.pacing.clone(),
            t0,
            drawable: None,
        })
    }
}

/// A drawable that the frame presents after its work.
pub(crate) struct PendingPresent {
    pub(crate) drawable: Retained<ProtocolObject<dyn MTLDrawable>>,
    pub(crate) keep: Box<dyn Any>,
    /// The time the display should show the frame, in seconds on the `CACurrentMediaTime` clock.
    pub(crate) at: Option<f64>,
}

type FeedbackBlock =
    RcBlock<'static, fn(NonNull<ProtocolObject<dyn MTL4CommitFeedback>>), dyn Send + Sync>;

type PresentedBlock =
    RcBlock<'static, fn(NonNull<ProtocolObject<dyn MTLDrawable>>), dyn Send + Sync>;

/// One frame being encoded.
pub struct Frame {
    number: u64,
    cb: CommandBuffer,
    scratch: Arc<Scratch>,
    done: Arc<Completion>,
    ctx: Context,
    metrics: Arc<Metrics>,
    hooks: Option<Arc<dyn FrameHooks>>,
    pacing: Option<Arc<PacingStats>>,
    t0: Instant,
    drawable: Option<PendingPresent>,
}

/// A frame that is on the GPU.
pub struct Submitted {
    number: u64,
    done: Arc<Completion>,
}

impl Submitted {
    /// The number of the frame.
    pub fn number(&self) -> u64 {
        self.number
    }

    /// Waits until the GPU finished the frame and returns its numbers.
    pub fn wait(&self) -> Result<CommandBufferStats> {
        let s = self.done.wait()?;
        if s.failed {
            return Err(MetalError::Gpu(format!("frame {} failed", self.number)));
        }
        Ok(s)
    }
}

impl Frame {
    /// The number of the frame, from 1.
    pub fn number(&self) -> u64 {
        self.number
    }

    /// The command buffer of the frame.
    pub fn commands(&mut self) -> &mut CommandBuffer {
        &mut self.cb
    }

    /// The scratch buffer of the frame. It is free again when the slot comes round.
    pub fn scratch(&self) -> Arc<Scratch> {
        self.scratch.clone()
    }

    pub(crate) fn set_present(&mut self, p: PendingPresent) {
        self.drawable = Some(p);
    }

    /// Ends the command buffer, commits it and returns at once.
    pub fn submit(mut self) -> Result<Submitted> {
        self.cb.raw().endCommandBuffer();
        let cpu_encode_ns = u64::try_from(self.t0.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let keep = self.cb.take_retained();
        let done = self.done.clone();
        let metrics = self.metrics.clone();
        let hooks = self.hooks.clone();
        let number = self.number;
        let keep = Mutex::new(Some(Keep::new((keep, self.scratch.clone()))));
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
                if let Ok(mut k) = keep.lock() {
                    k.take();
                }
                metrics.record(&stats);
                if let Some(h) = &hooks {
                    h.frame_complete(&stats);
                }
                done.finish(stats);
            });
        let opts = MTL4CommitOptions::new();
        opts.addFeedbackHandler(&handler);
        self.done.set_pending();
        let queue = self.ctx.queue();
        if let Some(p) = &self.drawable {
            if let Some(h) = &self.hooks {
                h.before_present(number);
            }
            queue.waitForDrawable(&p.drawable);
        }
        let ptr: NonNull<ProtocolObject<dyn MTL4CommandBuffer>> = NonNull::from(&**self.cb.raw());
        let list = [ptr];
        // SAFETY: `list` holds one valid, ended command buffer and the count says one.
        unsafe { queue.commit_count_options(NonNull::from(&list[0]), 1, &opts) };
        if let Some(p) = self.drawable.take() {
            queue.signalDrawable(&p.drawable);
            if let Some(stats) = &self.pacing {
                let (stats, at) = (stats.clone(), p.at);
                let presented: PresentedBlock =
                    RcBlock::new(move |d: NonNull<ProtocolObject<dyn MTLDrawable>>| {
                        // SAFETY: Metal passes a valid drawable that lives for the call.
                        let shown = unsafe { d.as_ref() }.presentedTime();
                        stats.record_presented(at, shown);
                    });
                p.drawable.addPresentedHandler(&presented);
            }
            // A drawable of `CAMetalDisplayLink` is shown at the target time of its tick, and `presentAtTime:`
            // raises `CAMetalDrawableInvalidOperation` on it. `p.at` stays for the pacing statistics.
            let drawable = p.drawable.clone();
            // With timed presents on, the display keeps the frame before this one on screen for the minimum time. If
            // Metal refuses that call for a drawable of the display link, timed presents go off for good and the
            // plain present follows.
            let timed = crate::present::timed_present();
            let mut sent = false;
            if let Some(min) = timed {
                // The closure only calls `presentAfterMinimumDuration:`, which leaves no Rust state half done when it throws.
                let r = objc2::exception::catch(std::panic::AssertUnwindSafe(|| drawable.presentAfterMinimumDuration(min)));
                match r {
                    Ok(()) => sent = true,
                    Err(e) => {
                        crate::present::set_timed_present(None);
                        eprintln!("nxkit-metal: timed presents are off: presentAfterMinimumDuration threw {e:?}");
                    }
                }
            }
            if !sent {
                // The closure only calls `present`, which leaves no Rust state half done when it throws.
                if let Err(e) = objc2::exception::catch(std::panic::AssertUnwindSafe(|| drawable.present())) {
                    static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                    if SHOWN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 4 {
                        eprintln!("nxkit-metal: [RED] present threw {e:?}");
                    }
                }
            }
            drop(p.keep);
        }
        Ok(Submitted {
            number: self.number,
            done: self.done.clone(),
        })
    }
}
