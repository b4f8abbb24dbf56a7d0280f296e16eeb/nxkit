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
//!
//! On a screen that adapts its refresh rate the host can give the link a [`RateRange`] and turn on the immediate
//! mode of the pacer ([`Pacer::set_immediate`]). A tick that finds no frame then keeps its drawable, and the frame that
//! comes next is drawn into it at once, so the screen shows a frame when it is ready and not on the next tick.

use std::cell::OnceCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
use crate::schedule::{Shown, TickAction, TickScheduler};
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

    /// The range of a fixed rate: minimum, maximum and preferred are the same.
    pub fn range(self) -> RateRange {
        let hz = self.hz();
        RateRange {
            min: hz,
            max: hz,
            preferred: hz,
        }
    }
}

/// The range of rates that the display link may run at, in frames per second.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RateRange {
    /// The lowest rate the game needs.
    pub min: f32,
    /// The highest rate, usually the highest rate of the screen.
    pub max: f32,
    /// The rate the link asks for.
    pub preferred: f32,
}

impl RateRange {
    /// The range for variable refresh: from `floor_hz` (what the game needs at least) to `screen_max_hz`, with `target_hz`
    /// preferred. The target is kept inside the screen's rates, and the floor never goes above the target.
    pub fn plan(floor_hz: u32, screen_max_hz: u32, target_hz: u32) -> Self {
        let max = screen_max_hz.max(1);
        let preferred = target_hz.clamp(1, max);
        let min = floor_hz.clamp(1, preferred);
        Self {
            min: min as f32,
            max: max as f32,
            preferred: preferred as f32,
        }
    }

    /// Whether the range is wider than one rate.
    pub fn is_variable(&self) -> bool {
        self.max > self.min
    }

    fn to_ca(self) -> CAFrameRateRange {
        CAFrameRateRange {
            minimum: self.min,
            maximum: self.max,
            preferred: self.preferred,
        }
    }
}

/// What the display link is asked for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LinkRate {
    /// One rate. The screen runs at it, and a frame shows on a tick of this grid.
    Fixed(FrameInterval),
    /// A range. The screen follows the frames that are presented.
    Variable(RateRange),
}

impl LinkRate {
    /// The range for `preferredFrameRateRange`.
    pub fn range(self) -> RateRange {
        match self {
            LinkRate::Fixed(i) => i.range(),
            LinkRate::Variable(r) => r,
        }
    }
}

impl From<FrameInterval> for LinkRate {
    fn from(i: FrameInterval) -> Self {
        LinkRate::Fixed(i)
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
    /// True in the immediate mode of the pacer: the frame is shown as soon as it is ready, so the times above are not
    /// a target (a parked drawable can be some milliseconds old). Present with `Frame::present`, not `Frame::present_at`.
    pub immediate: bool,
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
    /// The immediate mode: a tick without a frame keeps its drawable, and the next ready frame uses it at once.
    immediate: AtomicBool,
    /// The drawable of the newest tick that found no frame.
    parked: Mutex<Option<Parked>>,
    /// Held while a frame goes to the sink in the immediate mode, so that frames reach it in order.
    order: Mutex<()>,
    /// The frames that the immediate mode dropped because a newer one was ready before a tick could show them.
    superseded: AtomicU64,
}

/// A tick that waits for a frame.
struct Parked(DisplayTick);

// SAFETY: a `CAMetalDrawable` and its texture are Metal objects, which Metal allows to be used from any thread. The
// pacer moves a parked tick from the main thread to the thread of the next ready frame under a lock, and exactly one
// thread owns it at a time.
unsafe impl Send for Parked {}

impl Pacer {
    /// A pacer for a plan. Give it to [`DisplayLink::new`] as the hooks.
    pub fn new(plan: PresentPlan, sink: Arc<dyn TickSink>) -> Arc<Self> {
        Arc::new(Self {
            plan,
            scheduler: Mutex::new(TickScheduler::new(plan.source)),
            stats: Arc::new(PacingStats::default()),
            sink,
            immediate: AtomicBool::new(false),
            parked: Mutex::new(None),
            order: Mutex::new(()),
            superseded: AtomicU64::new(0),
        })
    }

    /// Turns the immediate mode on or off. In it the pacer ignores the hold of the source: a frame that is ready shows
    /// at once on the drawable of the newest tick, or on the next tick when none waits, and at most one frame is queued.
    /// Turning it off lets go of the parked drawable.
    pub fn set_immediate(&self, on: bool) {
        if self.immediate.swap(on, Ordering::AcqRel) != on && !on {
            *lock(&self.parked) = None;
        }
    }

    /// Whether the immediate mode is on.
    pub fn immediate(&self) -> bool {
        self.immediate.load(Ordering::Acquire)
    }

    /// The frames that the immediate mode dropped in favour of a newer one.
    pub fn superseded(&self) -> u64 {
        self.superseded.load(Ordering::Relaxed)
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
        if !self.immediate() {
            lock(&self.scheduler).frame_ready(frame);
            return;
        }
        let _order = lock(&self.order);
        let parked = {
            let mut sched = lock(&self.scheduler);
            sched.frame_ready(frame);
            match lock(&self.parked).take() {
                Some(p) => p,
                None => {
                    // No tick waits: the frame stays queued for the next one, and only the newest stays.
                    self.superseded
                        .fetch_add(sched.keep_newest(), Ordering::Relaxed);
                    return;
                }
            }
        };
        self.show_latest(parked.0, true);
    }

    /// Hands the newest ready frame to the sink on `tick`. `was_parked`: the tick was counted when it parked.
    fn show_latest(&self, tick: DisplayTick, was_parked: bool) {
        let latest = lock(&self.scheduler).take_latest();
        let Some((frame, dropped)) = latest else {
            *lock(&self.parked) = Some(Parked(tick));
            return;
        };
        self.superseded.fetch_add(dropped, Ordering::Relaxed);
        let action = TickAction::Present(Shown::Game { frame });
        if was_parked {
            self.stats.record_action_only(&action);
        } else {
            self.stats.record_tick(&action);
        }
        self.sink.tick(
            action,
            DisplayTick {
                immediate: true,
                ..tick
            },
        );
    }

    fn immediate_tick(&self, tick: DisplayTick) {
        let t0 = Instant::now();
        let _order = lock(&self.order);
        let has_frame = lock(&self.scheduler).queued() > 0;
        if has_frame {
            self.show_latest(tick, false);
        } else {
            self.stats.record_parked();
            *lock(&self.parked) = Some(Parked(tick));
        }
        let ns = u64::try_from(t0.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.stats.record_handler(ns);
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl FrameHooks for Pacer {
    fn display_tick(&self, tick: DisplayTick) {
        if self.immediate() {
            self.immediate_tick(tick);
            return;
        }
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
                    immediate: false,
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
        rate: impl Into<LinkRate>,
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
        link.setPreferredFrameRateRange(rate.into().range().to_ca());
        // SAFETY: the run loop and the mode are live objects. `Drop` removes the link again.
        unsafe { link.addToRunLoop_forMode(&NSRunLoop::currentRunLoop(), NSDefaultRunLoopMode) };
        Self {
            link,
            _target: target,
        }
    }

    /// Changes the target frame rate.
    pub fn set_interval(&self, interval: FrameInterval) {
        self.set_rate(interval);
    }

    /// Changes the rate or the range that the link asks for.
    pub fn set_rate(&self, rate: impl Into<LinkRate>) {
        self.link
            .setPreferredFrameRateRange(rate.into().range().to_ca());
    }

    /// The range the link asks for now.
    pub fn rate_range(&self) -> RateRange {
        let r = self.link.preferredFrameRateRange();
        RateRange {
            min: r.minimum,
            max: r.maximum,
            preferred: r.preferred,
        }
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
    fn the_range_for_variable_refresh_is_ordered_and_inside_the_screen() {
        let r = RateRange::plan(60, 120, 120);
        assert_eq!((r.min, r.max, r.preferred), (60.0, 120.0, 120.0));
        assert!(r.is_variable());
        // A target above the screen's highest rate is cut to it.
        let r = RateRange::plan(60, 120, 240);
        assert_eq!((r.min, r.max, r.preferred), (60.0, 120.0, 120.0));
        // A target under the floor lowers the floor, so min <= preferred <= max always holds.
        let r = RateRange::plan(60, 120, 48);
        assert_eq!((r.min, r.max, r.preferred), (48.0, 120.0, 48.0));
        // A 144 Hz Adaptive-Sync screen.
        let r = RateRange::plan(60, 144, 144);
        assert_eq!((r.min, r.max, r.preferred), (60.0, 144.0, 144.0));
        // Zeros do not make a range that Core Animation would refuse.
        let r = RateRange::plan(0, 0, 0);
        assert!(r.min >= 1.0 && r.min <= r.preferred && r.preferred <= r.max);
        assert!(!r.is_variable());
    }

    #[test]
    fn a_fixed_rate_is_a_range_of_one() {
        let r = LinkRate::from(FrameInterval::Hz60).range();
        assert_eq!((r.min, r.max, r.preferred), (60.0, 60.0, 60.0));
        assert!(!r.is_variable());
        assert_eq!(
            LinkRate::Variable(RateRange::plan(60, 120, 120))
                .range()
                .max,
            120.0
        );
    }

    struct Record(Mutex<Vec<(TickAction, bool)>>);
    impl TickSink for Record {
        fn tick(&self, action: TickAction, tick: DisplayTick) {
            lock(&self.0).push((action, tick.immediate));
        }
    }

    #[test]
    fn the_immediate_mode_shows_a_frame_on_the_parked_tick() {
        let Ok(ctx) = Context::system_default() else {
            eprintln!("skipped: no Metal device");
            return;
        };
        let presenter = Presenter::offscreen(&ctx, 64, 64);
        let tick = |p: &Presenter| -> Result<DisplayTick, ()> {
            Ok(DisplayTick {
                drawable: p.acquire().map_err(|_| ())?,
                target_timestamp: 0.0,
                target_presentation_timestamp: 0.0,
                immediate: false,
            })
        };
        let sink = Arc::new(Record(Mutex::new(Vec::new())));
        let plan = crate::policy::choose_present_plan(&crate::policy::PolicyInput {
            title: None,
            frame_step: false,
            game_hz: 120,
            display: FrameInterval::Hz120,
            interpolation: false,
        });
        let pacer = Pacer::new(plan, sink.clone());
        pacer.set_immediate(true);
        let shown = |n: usize| lock(&sink.0).len() == n;
        // A tick without a frame parks its drawable and shows nothing.
        let Ok(t) = tick(&presenter) else { return };
        pacer.display_tick(t);
        assert!(shown(0));
        assert_eq!(pacer.stats().snapshot().parked, 1);
        // The frame that comes next shows at once, without a new tick.
        pacer.game_frame_ready(1);
        assert_eq!(
            lock(&sink.0).as_slice(),
            &[(TickAction::Present(Shown::Game { frame: 1 }), true)]
        );
        // No tick waits now: two frames queue, only the newest stays, and the next tick shows it.
        pacer.game_frame_ready(2);
        pacer.game_frame_ready(3);
        assert!(shown(1));
        assert_eq!(pacer.superseded(), 1);
        let Ok(t) = tick(&presenter) else { return };
        pacer.display_tick(t);
        assert_eq!(
            lock(&sink.0)[1],
            (TickAction::Present(Shown::Game { frame: 3 }), true)
        );
        // Off again: ticks follow the source as before.
        pacer.set_immediate(false);
        let Ok(t) = tick(&presenter) else { return };
        pacer.display_tick(t);
        assert_eq!(
            lock(&sink.0)[2],
            (TickAction::Missed, false),
            "a tick of the grid with no frame"
        );
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
        let range = RateRange::plan(60, 120, 120);
        link.set_rate(LinkRate::Variable(range));
        assert_eq!(link.rate_range(), range);
        assert_eq!(link.target_hz(), 120.0);
        link.set_paused(true);
    }
}
