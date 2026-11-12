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
    /// Whether variable refresh is permitted when verified by the caller.
    pub vrr: bool,
    /// Lowest acceptable adaptive refresh rate.
    pub vrr_floor: u32,
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
            vrr: std::env::var("NXKIT_VRR").map_or(true, |s| s.trim() != "0"),
            vrr_floor: positive_env("NXKIT_VRR_MIN").unwrap_or(60),
        }
    }
}
/// Whether frames follow a fixed requested grid or a verified adaptive display.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Presentation {
    /// Frames follow a fixed cadence.
    Grid,
    /// Frames follow the verified adaptive range.
    Vrr(RateRange),
}
/// A variable game step requires verified adaptive support, not just a fast panel.
pub fn choose_presentation(
    request: &Request,
    adaptive: bool,
    variable_target: Option<u32>,
    max_fps: u32,
) -> Presentation {
    match variable_target {
        Some(target) if adaptive && request.vrr && max_fps > request.vrr_floor => {
            Presentation::Vrr(RateRange::plan(request.vrr_floor, max_fps, target))
        }
        _ => Presentation::Grid,
    }
}
struct State {
    request: Request,
    variable_target: Option<u32>,
    grid_step: Option<StepMode>,
    presentation: Presentation,
    plan: Option<TickPlan>,
    applied_hz: f64,
}
static STATE: Mutex<State> = Mutex::new(State {
    request: Request {
        target_fps: 60,
        free_rate: false,
        set_refresh: None,
        timed: None,
        vrr: true,
        vrr_floor: 60,
    },
    variable_target: None,
    grid_step: None,
    presentation: Presentation::Grid,
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
    st.variable_target = target;
    st.grid_step = Some(grid_step);
}
/// Increases whenever a requested screen configuration is applied.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}
/// Returns the current fixed or adaptive presentation policy.
pub fn presentation() -> Presentation {
    state().presentation
}
/// Returns the verified adaptive range, if one is active.
pub fn vrr_range() -> Option<RateRange> {
    match presentation() {
        Presentation::Vrr(r) => Some(r),
        Presentation::Grid => None,
    }
}
/// Returns the most recently applied tick plan.
pub fn plan() -> Option<TickPlan> {
    state().plan
}
fn maximum_or_default(maximum_fps: u32) -> u32 {
    if maximum_fps == 0 { 60 } else { maximum_fps }
}
fn tick_plan(request: &Request, maximum_fps: u32, adaptive: bool) -> TickPlan {
    let maximum = maximum_or_default(maximum_fps);
    // Do not ask the link for more than UIKit's reported maximum.
    let target = request.target_fps.max(1).min(maximum);
    refresh::plan_ticks(f64::from(maximum), target, request.free_rate, adaptive)
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
    apply_screen(maximum_fps, false)
}
/// Requests adaptive pacing only when the caller has established that capability.
/// The returned range belongs on CAMetalDisplayLink.preferredFrameRateRange.
/// Call again when screen capabilities change; measurements handle lower delivery.
pub fn apply_screen(maximum_fps: u32, adaptive_verified: bool) -> LinkRate {
    let mut st = state();
    let req = st.request;
    let maximum = maximum_or_default(maximum_fps);
    let plan = tick_plan(&req, maximum, adaptive_verified);
    let presentation = choose_presentation(&req, adaptive_verified, st.variable_target, maximum);
    let vrr = match presentation {
        Presentation::Grid => None,
        Presentation::Vrr(r) => Some(r),
    };
    let rate = vrr.map_or(
        LinkRate::Fixed(FrameInterval::from_hz(plan.link_hz)),
        LinkRate::Variable,
    );
    let requested_hz = rate.range().preferred;
    if let Err(error) = nxkit_svc_pacing::set_rates(requested_hz, vsync_for(plan, req)) {
        eprintln!("nx2ipa: cannot configure frame pacing: {error:?}");
    }
    nxkit_svc_pacing::set_screen_hz(maximum);
    // This is initially the requested rate; actual fixed delivery replaces it below.
    nxkit_nvn::screen::set_refresh_hz(f64::from(requested_hz));
    nxkit_svc_pacing::set_step_by_shown(vrr.is_some());
    if let (Some(target), Some(grid)) = (st.variable_target, st.grid_step) {
        let step = if vrr.is_some() {
            StepMode::Variable(target)
        } else {
            grid
        };
        if let Err(error) = nxkit_svc_pacing::set_step_mode(step) {
            eprintln!("nx2ipa: cannot configure frame step: {error:?}");
        }
    }
    nxkit_metal::set_timed_present(
        (req.timed == Some(true) && vrr.is_none()).then(|| 1.0 / f64::from(plan.fps.max(1))),
    );
    if req.set_refresh.is_some() {
        eprintln!(
            "nx2ipa: display mode switching is unavailable on iOS; NXKIT_SET_REFRESH is not applied"
        );
    }
    st.plan = Some(plan);
    st.presentation = presentation;
    st.applied_hz = f64::from(requested_hz);
    GENERATION.fetch_add(1, Ordering::Relaxed);
    rate
}
fn meaningful_measurement(measured: f64, previous: f64) -> bool {
    measured.is_finite() && measured >= 1.0 && refresh::rate_differs(measured, previous)
}
/// Corrects the fixed-grid hold rate using measured ticks, including recovery from
/// a temporary power/thermal reduction. Adaptive ticks have no fixed expectation.
pub fn note_measured(measured_hz: f64) -> Option<f64> {
    let mut st = state();
    let plan = st.plan?;
    if matches!(st.presentation, Presentation::Vrr(_))
        || !meaningful_measurement(measured_hz, st.applied_hz)
    {
        return None;
    }
    let hz = measured_hz.round();
    if let Err(error) = nxkit_svc_pacing::set_rates(hz as f32, vsync_for(plan, st.request)) {
        eprintln!("nx2ipa: cannot apply measured frame rate: {error:?}");
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
            vrr: true,
            vrr_floor: 60,
        }
    }
    #[test]
    fn a_fast_screen_does_not_prove_adaptive_support() {
        assert_eq!(
            choose_presentation(&request(), false, Some(120), 120),
            Presentation::Grid
        );
        let p = tick_plan(&request(), 120, false);
        assert_eq!((p.link_hz, p.ticks_per_frame), (120, 2));
    }
    #[test]
    fn adaptive_range_requires_a_variable_game_and_room_above_floor() {
        assert_eq!(
            choose_presentation(&request(), true, None, 120),
            Presentation::Grid
        );
        assert_eq!(
            choose_presentation(&request(), true, Some(120), 60),
            Presentation::Grid
        );
        assert_eq!(
            choose_presentation(
                &Request {
                    vrr: false,
                    ..request()
                },
                true,
                Some(120),
                120
            ),
            Presentation::Grid
        );
        let Presentation::Vrr(r) = choose_presentation(&request(), true, Some(240), 120) else {
            panic!("expected range")
        };
        assert_eq!((r.min, r.max, r.preferred), (60.0, 120.0, 120.0));
    }
    #[test]
    fn rate_requests_respect_the_reported_ceiling() {
        assert_eq!(tick_plan(&request(), 30, false).link_hz, 30);
        assert_eq!(tick_plan(&request(), 0, false).link_hz, 60);
        assert_eq!(
            tick_plan(
                &Request {
                    target_fps: 30,
                    ..request()
                },
                120,
                false
            )
            .ticks_per_frame,
            4
        );
        assert!(!tick_plan(&request(), 144, false).exact);
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
