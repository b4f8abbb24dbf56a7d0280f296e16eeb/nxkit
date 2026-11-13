//! iOS frame-rate requests and feedback from actual CAMetalDisplayLink ticks.
//!
//! UIKit reports a maximum, not the current panel rate or adaptive capability.
//! Requests are advisory: low-power/thermal policy can reduce the delivered rate.
//! We never switch a desktop display mode or infer VRR from a high maximum alone.
use crate::settings::Settings;
use nxkit_metal::{FrameInterval, LinkRate, RateRange};
use nxkit_svc_pacing::{
    StepMode,
    refresh::{self, TickPlan},
};
use std::sync::{
    Mutex, MutexGuard,
    atomic::{AtomicU64, Ordering},
};

const VSYNC_HZ: f32 = 60.0;
/// User/game pacing preferences. Display-mode changes are unsupported on iOS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    /// Preferred guest frame rate.
    pub target_fps: u32,
    /// Whether the game permits rates outside its preferred cadence.
    pub free_rate: bool,
    /// Desktop mode request, retained for diagnostics but not applied on iOS.
    pub set_refresh: Option<u32>,
    /// Explicit timed-presentation preference.
    pub timed: Option<bool>,
}
impl Request {
    /// Combines game settings with the existing pacing environment overrides.
    pub fn from_settings(settings: &Settings) -> Self {
        let positive_env = |name| {
            std::env::var(name)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
                .filter(|n| *n > 0)
        };
        Self {
            target_fps: if settings.fps_target == 0 {
                60
            } else {
                settings.fps_target.min(60)
            },
            free_rate: settings.fps_free,
            set_refresh: positive_env("NXKIT_SET_REFRESH")
                .or((settings.set_refresh > 0).then_some(settings.set_refresh)),
            timed: std::env::var("NXKIT_TIMED_PRESENT")
                .ok()
                .and_then(|s| match s.trim() {
                    "0" => Some(false),
                    "1" => Some(true),
                    _ => None,
                }),
        }
    }
}
struct State {
    request: Request,
    grid_step: Option<StepMode>,
    plan: Option<TickPlan>,
    applied_hz: f64,
}
static STATE: Mutex<State> = Mutex::new(State {
    request: Request {
        target_fps: 60,
        free_rate: false,
        set_refresh: None,
        timed: None,
    },
    grid_step: None,
    plan: None,
    applied_hz: 0.0,
});
static GENERATION: AtomicU64 = AtomicU64::new(0);
fn state() -> MutexGuard<'static, State> {
    STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
/// Configure before the session installs its display link.
pub fn configure(request: Request) {
    state().request = request;
}
/// Called by the unchanged pacing seam. Fixed displays must use the grid fallback.
pub fn set_variable_step(target: Option<u32>, grid_step: StepMode) {
    let mut st = state();
    st.grid_step = target.map(|_| grid_step);
}
/// Increases whenever a requested screen configuration is applied.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}
/// iOS uses the fixed-grid presentation path.
pub fn vrr_range() -> Option<RateRange> {
    None
}
fn maximum_or_default(maximum_fps: u32) -> u32 {
    if maximum_fps == 0 { 60 } else { maximum_fps }
}
fn tick_plan(request: &Request, maximum_fps: u32) -> TickPlan {
    let maximum = maximum_or_default(maximum_fps);
    // Do not ask the link for more than UIKit's reported maximum.
    let target = request.target_fps.max(1).min(maximum);
    refresh::plan_ticks(f64::from(maximum), target, request.free_rate, false)
}
fn vsync_for(plan: TickPlan, request: Request) -> f32 {
    if plan.fps == request.target_fps {
        VSYNC_HZ
    } else {
        plan.fps.max(1) as f32
    }
}
/// Plans a fixed rate using UIKit's maximum. This does not establish VRR support.
pub fn apply(maximum_fps: u32) -> LinkRate {
    let mut st = state();
    let req = st.request;
    let maximum = maximum_or_default(maximum_fps);
    let plan = tick_plan(&req, maximum);
    let rate = LinkRate::Fixed(FrameInterval::from_hz(plan.link_hz));
    let requested_hz = rate.range().preferred;
    if let Err(error) = nxkit_svc_pacing::set_rates(requested_hz, vsync_for(plan, req)) {
        eprintln!("nxkit: cannot configure frame pacing: {error:?}");
    }
    nxkit_svc_pacing::set_screen_hz(maximum);
    // This is initially the requested rate; actual fixed delivery replaces it below.
    nxkit_nvn::screen::set_refresh_hz(f64::from(requested_hz));
    nxkit_svc_pacing::set_step_by_shown(false);
    if let Some(step) = st.grid_step {
        if let Err(error) = nxkit_svc_pacing::set_step_mode(step) {
            eprintln!("nxkit: cannot configure frame step: {error:?}");
        }
    }
    nxkit_metal::set_timed_present(
        (req.timed == Some(true)).then(|| 1.0 / f64::from(plan.fps.max(1))),
    );
    if req.set_refresh.is_some() {
        eprintln!(
            "nxkit: display mode switching is unavailable on iOS; NXKIT_SET_REFRESH is not applied"
        );
    }
    st.plan = Some(plan);
    st.applied_hz = f64::from(requested_hz);
    GENERATION.fetch_add(1, Ordering::Relaxed);
    rate
}
fn meaningful_measurement(measured: f64, previous: f64) -> bool {
    measured.is_finite() && measured >= 1.0 && refresh::rate_differs(measured, previous)
}
/// Corrects the fixed-grid hold rate using measured ticks, including recovery from
/// a temporary power/thermal reduction.
pub fn note_measured(measured_hz: f64) -> Option<f64> {
    let mut st = state();
    let plan = st.plan?;
    if !meaningful_measurement(measured_hz, st.applied_hz) {
        return None;
    }
    let hz = measured_hz.round();
    if let Err(error) = nxkit_svc_pacing::set_rates(hz as f32, vsync_for(plan, st.request)) {
        eprintln!("nxkit: cannot apply measured frame rate: {error:?}");
        return None;
    }
    nxkit_svc_pacing::set_screen_hz(hz as u32);
    nxkit_nvn::screen::set_refresh_hz(hz);
    st.applied_hz = hz;
    Some(measured_hz)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> Request {
        Request {
            target_fps: 60,
            free_rate: false,
            set_refresh: None,
            timed: None,
        }
    }
    #[test]
    fn a_fast_screen_uses_fixed_ticks() {
        let p = tick_plan(&request(), 120);
        assert_eq!((p.link_hz, p.ticks_per_frame), (120, 2));
    }
    #[test]
    fn rate_requests_respect_the_reported_ceiling() {
        assert_eq!(tick_plan(&request(), 30).link_hz, 30);
        assert_eq!(tick_plan(&request(), 0).link_hz, 60);
        assert_eq!(
            tick_plan(
                &Request {
                    target_fps: 30,
                    ..request()
                },
                120
            )
            .ticks_per_frame,
            4
        );
        assert!(!tick_plan(&request(), 144).exact);
    }
    #[test]
    fn measured_feedback_rejects_invalid_values_and_tracks_recovery() {
        for bad in [f64::NAN, f64::INFINITY, -1.0, 0.0] {
            assert!(!meaningful_measurement(bad, 60.0));
        }
        assert!(!meaningful_measurement(59.94, 60.0));
        assert!(meaningful_measurement(60.0, 120.0));
        assert!(meaningful_measurement(120.0, 60.0));
    }
}
