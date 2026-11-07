//! Frame pacing with `CAMetalDisplayLink`.
//!
//! A [`DisplayLink`] calls [`FrameHooks::display_tick`] once per display refresh at the chosen rate. Each tick
//! carries the drawable that the layer reserved for it. `CAMetalDisplayLink` is available on macOS 14 and later.
//! There is no `CADisplayLink` fallback, because the SDK of this crate has `CAMetalDisplayLink` and the host
//! targets macOS 26 and later.

use std::cell::OnceCell;
use std::sync::Arc;

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
use crate::present::{Drawable, Presenter};

/// The target frame rate of a [`DisplayLink`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameInterval {
    /// 60 frames per second.
    Hz60,
    /// 120 frames per second.
    Hz120,
}

impl FrameInterval {
    /// The frames per second.
    pub fn hz(self) -> f32 {
        match self {
            FrameInterval::Hz60 => 60.0,
            FrameInterval::Hz120 => 120.0,
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
        link.set_paused(true);
    }
}
