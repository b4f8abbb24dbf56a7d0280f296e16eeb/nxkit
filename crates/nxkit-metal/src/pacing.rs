//! Frame pacing with `CAMetalDisplayLink`.
//!
//! A [`DisplayLink`] calls [`FrameHooks::display_tick`] once per display refresh at the chosen rate. Each tick
//! carries the drawable that the layer reserved for it. `CAMetalDisplayLink` is available on macOS 14 and later.
//! There is no `CADisplayLink` fallback, because the SDK of this crate has `CAMetalDisplayLink` and the host
//! targets macOS 26 and later.
//!
//! A [`Pacer`] is the `FrameHooks` that a host gives to the link. It takes the finished game frames
//! (`Pacer::game_frame_ready`), asks the [`TickScheduler`] what each tick shows and hands that to a [`TickSink`]. The
//! sink encodes the frame and calls `Frame::present_at` with `DisplayTick::target_presentation_timestamp`, so the
//! display shows the frame on that tick and on no other. Frames are never blended.

use std::cell::OnceCell;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AllocAnyThread, Ivars, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_foundation::{NSDefaultRunLoopMode, NSRunLoop};
use objc2_foundation::{NSObject, NSObjectProtocol};
use objc2_quartz_core::{
    CAFrameRateRange, CAMetalDisplayLink, CAMetalDisplayLinkDelegate, CAMetalDisplayLinkUpdate,
};

use crate::device::Context;
use crate::metrics::FrameHooks;
use crate::policy::{FrameSource, PresentPlan};
use crate::present::{Drawable, Presenter};
use crate::schedule::{TickAction, TickScheduler};
use crate::stats::PacingStats;

/// The target frame rate of a [`DisplayLink`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameInterval {
    /// 60 frames per second.
    Hz60,
    /// 120 frames per second.
    Hz120,
    /// Any other whole rate, for example 72 or 144. [`FrameInterval::from_hz`] makes it.
    Hz(u32),
}

impl FrameInterval {
    /// The interval for a rate in Hz, at least 1.
    pub fn from_hz(hz: u32) -> Self {
        match hz {
            60 => FrameInterval::Hz60,
            120 => FrameInterval::Hz120,
            other => FrameInterval::Hz(other.max(1)),
        }
    }

    /// The frames per second.
    pub fn hz(self) -> f32 {
        match self {
            FrameInterval::Hz60 => 60.0,
            FrameInterval::Hz120 => 120.0,
            FrameInterval::Hz(hz) => hz as f32,
        }
    }

    fn range(self) -> CAFrameRateRange {
        let hz = self.hz();
        CAFrameRateRange {
            minimum: hz,
            maximum: hz,
            preferred: hz,
        }
    }
}

/// What a display tick carries.
pub struct DisplayTick {
    /// The drawable for this tick.
    pub drawable: Drawable,
    /// The time the display wants the frame to be ready by, in seconds on the `CACurrentMediaTime` clock.
    pub target_timestamp: f64,
    /// The time the frame is shown, in seconds on the same clock.
    pub target_presentation_timestamp: f64,
}

/// Takes the decision of each tick. The host implements it.
///
/// It runs on the main thread, inside the tick. Keep it short: encode the frame, call `Frame::present_at` with
/// `tick.target_presentation_timestamp` and submit. For `TickAction::Hold` and `TickAction::Missed` the tick's
/// drawable is not presented and the display keeps the last frame.
pub trait TickSink: Send + Sync {
    /// A tick of the display link and what it shows.
    fn tick(&self, action: TickAction, tick: DisplayTick);
}

/// Drives the ticks of a [`DisplayLink`] by a [`PresentPlan`].
pub struct Pacer {
    plan: PresentPlan,
    scheduler: Mutex<TickScheduler>,
    stats: Arc<PacingStats>,
    sink: Arc<dyn TickSink>,
}

impl Pacer {
    /// A pacer for a plan. Give it to [`DisplayLink::new`] as the hooks.
    pub fn new(plan: PresentPlan, sink: Arc<dyn TickSink>) -> Arc<Self> {
        Arc::new(Self {
            plan,
            scheduler: Mutex::new(TickScheduler::new(plan.source)),
            stats: Arc::new(PacingStats::default()),
            sink,
        })
    }

    /// The plan the pacer follows.
    pub fn plan(&self) -> PresentPlan {
        self.plan
    }

    /// The counters of the pacer. Give them to `FrameRing::set_pacing_stats` to also count shown frames.
    pub fn stats(&self) -> Arc<PacingStats> {
        self.stats.clone()
    }

    /// Changes what the ticks show, for example when the game changes its present interval.
    pub fn set_source(&self, source: FrameSource) {
        self.scheduler
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .set_source(source);
    }

    /// The game has a finished frame. It can be called from any thread, for example from the frame complete hook.
    pub fn game_frame_ready(&self, frame: u64) {
        self.scheduler
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .frame_ready(frame);
    }
}

impl FrameHooks for Pacer {
    fn display_tick(&self, tick: DisplayTick) {
        let t0 = Instant::now();
        let action = self
            .scheduler
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .on_tick();
        self.stats.record_tick(&action);
        self.sink.tick(action, tick);
        let ns = u64::try_from(t0.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.stats.record_handler(ns);
    }
}

define_class!(
    // SAFETY:
    // - `NSObject` has no subclassing requirements.
    // - `MainThreadOnly` is correct, because a display link calls its delegate on the run loop it was added to.
    // - `TickTarget` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    struct TickTarget {
        hooks: OnceCell<Arc<dyn FrameHooks>>,
        ctx: OnceCell<Context>,
    }

    // SAFETY: `NSObjectProtocol` has no extra requirements.
    unsafe impl NSObjectProtocol for TickTarget {}

    // SAFETY: the method has the signature of the protocol.
    unsafe impl CAMetalDisplayLinkDelegate for TickTarget {
        #[unsafe(method(metalDisplayLink:needsUpdate:))]
        fn metal_display_link_needs_update(
            &self,
            _link: &CAMetalDisplayLink,
            update: &CAMetalDisplayLinkUpdate,
        ) {
            if let (Some(hooks), Some(ctx)) = (self.hooks().get(), self.ctx().get()) {
                hooks.display_tick(DisplayTick {
                    drawable: Drawable::wrap(ctx, update.drawable()),
                    target_timestamp: update.targetTimestamp(),
                    target_presentation_timestamp: update.targetPresentationTimestamp(),
                });
            }
        }
    }
);

/// A display link on the run loop of the main thread. It stops when dropped.
pub struct DisplayLink {
    link: Retained<CAMetalDisplayLink>,
    _target: Retained<TickTarget>,
}

impl DisplayLink {
    /// Starts a link for the layer of `presenter` on the current run loop, which is the main one. The loop must run for ticks to come.
    pub fn new(
        mtm: MainThreadMarker,
        presenter: &Presenter,
        interval: FrameInterval,
        hooks: Arc<dyn FrameHooks>,
    ) -> Self {
        let this = TickTarget::alloc(mtm).set_ivars(Ivars::<TickTarget> {
            hooks: OnceCell::from(hooks),
            ctx: OnceCell::from(presenter.context().clone()),
        });
        // SAFETY: `init` of `NSObject` on a freshly allocated instance whose ivars are set.
        let target: Retained<TickTarget> = unsafe { msg_send![super(this), init] };
        let link =
            CAMetalDisplayLink::initWithMetalLayer(CAMetalDisplayLink::alloc(), presenter.layer());
        link.setDelegate(Some(ProtocolObject::from_ref(&*target)));
        link.setPreferredFrameLatency(2.0);
        link.setPreferredFrameRateRange(interval.range());
        // SAFETY: the run loop and the mode are live objects. `Drop` removes the link again.
        unsafe { link.addToRunLoop_forMode(&NSRunLoop::currentRunLoop(), NSDefaultRunLoopMode) };
        Self {
            link,
            _target: target,
        }
    }

    /// Changes the target frame rate.
    pub fn set_interval(&self, interval: FrameInterval) {
        self.link.setPreferredFrameRateRange(interval.range());
    }

    /// The target frame rate in frames per second.
    pub fn target_hz(&self) -> f32 {
        self.link.preferredFrameRateRange().preferred
    }

    /// Stops or resumes the ticks.
    pub fn set_paused(&self, paused: bool) {
        self.link.setPaused(paused);
    }
}

impl Drop for DisplayLink {
    fn drop(&mut self) {
        self.link.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Quiet;
    impl FrameHooks for Quiet {}

    #[test]
    fn any_whole_rate_makes_an_interval() {
        assert_eq!(FrameInterval::from_hz(60), FrameInterval::Hz60);
        assert_eq!(FrameInterval::from_hz(120), FrameInterval::Hz120);
        assert_eq!(FrameInterval::from_hz(144).hz(), 144.0);
        assert_eq!(FrameInterval::from_hz(0).hz(), 1.0);
    }

    #[test]
    fn the_target_rate_is_selectable() {
        let Ok(ctx) = Context::system_default() else {
            eprintln!("skipped: no Metal device");
            return;
        };
        let presenter = Presenter::offscreen(&ctx, 64, 64);
        // SAFETY: the test never runs the run loop and never receives a tick, so no delegate call happens on a
        // thread other than the one that made the link.
        let mtm = unsafe { MainThreadMarker::new_unchecked() };
        let link = DisplayLink::new(mtm, &presenter, FrameInterval::Hz60, Arc::new(Quiet));
        assert_eq!(link.target_hz(), 60.0);
        link.set_interval(FrameInterval::Hz120);
        assert_eq!(link.target_hz(), 120.0);
        link.set_interval(FrameInterval::from_hz(72));
        assert_eq!(link.target_hz(), 72.0);
        link.set_paused(true);
    }
}
