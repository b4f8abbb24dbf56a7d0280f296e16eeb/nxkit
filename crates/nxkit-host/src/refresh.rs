//! The refresh rate of the screen that shows the game, and what the display link does about it.
//!
//! `sys/appkit.rs` reads the facts of the screen the window is on and calls [`apply`] at the start, when the window
//! moves to another screen and when the display mode changes. `apply` finds the refresh rate of the current mode, plans
//! the ticks of the link per game frame (`nxkit_svc_pacing::refresh`), tells the pacing service and the judder counters,
//! and prints one line when anything changed. [`note_measured`] checks the plan against the real tick rate.
//! The design is in `docs/pacing.md`.

use std::sync::Mutex;

use nxkit_metal::display;
use nxkit_svc_pacing::refresh::{self, TickPlan};

use crate::settings::Settings;

/// The vsync of the game's frame step. A Switch game steps in vsyncs of 1/60 s.
const VSYNC_HZ: f32 = 60.0;

/// What the user and the game ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    /// The frame rate the game aims at.
    pub target_fps: u32,
    /// Whether any frame rate is fine for the game.
    pub free_rate: bool,
    /// The refresh rate to switch the display to, when set.
    pub set_refresh: Option<u32>,
    /// Timed presents: `Some(true)` forces them, `Some(false)` forbids them, `None` uses them on a screen that adapts its rate.
    pub timed: Option<bool>,
}

impl Request {
    /// The request of `settings` and the environment. `NXKIT_SET_REFRESH` and `NXKIT_TIMED_PRESENT` win over the keys.
    pub fn from_settings(settings: &Settings) -> Self {
        let env_refresh = std::env::var("NXKIT_SET_REFRESH").ok().and_then(|v| v.trim().parse::<u32>().ok());
        let set_refresh = env_refresh.or((settings.set_refresh > 0).then_some(settings.set_refresh)).filter(|hz| *hz > 0);
        let timed = std::env::var("NXKIT_TIMED_PRESENT").ok().and_then(|v| match v.trim() {
            "1" => Some(true),
            "0" => Some(false),
            _ => None,
        });
        Self {
            target_fps: if settings.fps_target == 0 { 60 } else { settings.fps_target.min(60) },
            free_rate: settings.fps_free,
            set_refresh,
            timed,
        }
    }
}

/// What the host knows of a screen.
#[derive(Clone, Debug, PartialEq)]
pub struct ScreenFacts {
    /// The name of the screen.
    pub name: String,
    /// The CoreGraphics display id, when it is known.
    pub display: Option<u32>,
    /// `NSScreen.maximumFramesPerSecond`.
    pub max_fps: u32,
    /// Whether the screen adapts its refresh rate (ProMotion).
    pub adaptive: bool,
}

struct State {
    request: Request,
    plan: Option<TickPlan>,
    announced: String,
    /// The display ids that a switch was tried for with this rate, so that a failed switch does not repeat.
    tried: Vec<(u32, u32)>,
    /// The measured rates already reported, rounded to Hz.
    reported: u32,
}

static STATE: Mutex<State> = Mutex::new(State {
    request: Request { target_fps: 60, free_rate: false, set_refresh: None, timed: None },
    plan: None,
    announced: String::new(),
    tried: Vec::new(),
    reported: 0,
});

/// The number of [`apply`] calls. The tick hook measures anew when it changes.
static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The number of plans made so far.
pub fn generation() -> u64 {
    GENERATION.load(std::sync::atomic::Ordering::Relaxed)
}

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Sets what is asked for. The host calls it once before the window opens.
pub fn configure(request: Request) {
    state().request = request;
}

/// The plan of the latest [`apply`].
pub fn plan() -> Option<TickPlan> {
    state().plan
}

/// The refresh rate of the current mode of the screen, and where it came from.
fn refresh_of(facts: &ScreenFacts) -> (f64, &'static str) {
    if let Some(hz) = facts.display.and_then(display::current_refresh_hz) {
        return (hz, "CoreGraphics mode");
    }
    (f64::from(facts.max_fps.max(1)), if facts.adaptive { "adaptive, highest rate" } else { "NSScreen" })
}

/// Plans for `facts`. It switches the display when `NXKIT_SET_REFRESH` asks, tells the pacing service the rates, turns
/// timed presents on or off and prints one line when the screen, the rate or the plan changed. Returns the plan.
pub fn apply(facts: &ScreenFacts) -> TickPlan {
    let mut st = state();
    let req = st.request;
    let mut note = String::new();
    if let (Some(want), Some(id)) = (req.set_refresh, facts.display)
        && !facts.adaptive
        && !st.tried.contains(&(id, want))
    {
        st.tried.push((id, want));
        match display::switch_refresh(id, f64::from(want)) {
            Ok(Some(sw)) => {
                note = format!("; switched the display from {:.2} Hz to {:.2} Hz, restored at exit", sw.from_hz, sw.to_hz);
                display::keep_active(sw);
            }
            Ok(None) => note = format!("; the display shows {want} Hz already"),
            Err(e) => eprintln!("nxkit-host: NXKIT_SET_REFRESH={want}: {e}; the refresh stays"),
        }
    }
    let (hz, source) = refresh_of(facts);
    let plan = refresh::plan_ticks(hz, req.target_fps, req.free_rate, facts.adaptive);
    let fps = plan.fps as f32;
    // The service takes the link rate and the vsync rate of the game, so that a frame step of N vsyncs holds N * ticks.
    let _ = nxkit_svc_pacing::set_rates(plan.link_hz as f32, if plan.fps == req.target_fps { VSYNC_HZ } else { fps });
    nxkit_svc_pacing::set_screen_hz(hz.round() as u32);
    nxkit_nvn::screen::set_refresh_hz(if facts.adaptive { f64::from(plan.link_hz) } else { hz });
    let timed = req.timed.unwrap_or(facts.adaptive);
    nxkit_metal::set_timed_present(timed.then(|| 1.0 / f64::from(plan.fps.max(1))));
    let mut hint = String::new();
    if !plan.exact {
        let choices = facts.display.map(display::refresh_choices).unwrap_or_default();
        let near = display::even_choice(&choices, req.target_fps, hz);
        let pick = near.map_or(plan.switch_to.map(f64::from), Some);
        if let Some(p) = pick {
            hint = format!(
                "; {hz:.0} Hz is not an even multiple of {} fps, so frames alternate between two lengths (judder): set NXKIT_SET_REFRESH={}",
                req.target_fps,
                p.round()
            );
        }
    }
    let line = format!(
        "nxkit-host: screen \"{}\" refresh {hz:.2} Hz ({source}{}), link {} Hz, {} tick{} per {} fps frame{}{}{}{note}{hint}",
        facts.name,
        if facts.adaptive { ", adaptive" } else { "" },
        plan.link_hz,
        plan.ticks_per_frame,
        if plan.ticks_per_frame == 1 { "" } else { "s" },
        plan.fps,
        if plan.exact { "" } else { " (uneven)" },
        if timed { ", timed presents" } else { "" },
        if req.free_rate && plan.fps != req.target_fps { ", free frame rate" } else { "" },
    );
    if line != st.announced {
        eprintln!("{line}");
        st.announced = line;
    }
    st.plan = Some(plan);
    st.reported = 0;
    GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    plan
}

/// The link ticked at `measured_hz` over the last second while the plan asked for `plan.link_hz`. When they differ by
/// more than 4%, the service gets the measured rate, so that a hold counts the ticks that really come. Returns the rate
/// when it changed the service.
pub fn note_measured(measured_hz: f64) -> Option<f64> {
    let mut st = state();
    let plan = st.plan?;
    let rounded = measured_hz.round() as u32;
    if !refresh::rate_differs(measured_hz, f64::from(plan.link_hz)) || rounded == st.reported || rounded == 0 {
        return None;
    }
    st.reported = rounded;
    let req = st.request;
    let vsync = if plan.fps == req.target_fps { VSYNC_HZ } else { plan.fps as f32 };
    let _ = nxkit_svc_pacing::set_rates(rounded as f32, vsync);
    eprintln!(
        "nxkit-host: the display link ticks at {measured_hz:.1} Hz, not the {} Hz that was asked for, so a frame holds {:.1} ticks per vsync now",
        plan.link_hz,
        measured_hz / f64::from(vsync)
    );
    Some(measured_hz)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_takes_the_target_from_the_settings() {
        let s = Settings { fps_target: 30, fps_free: true, set_refresh: 120, ..Settings::default() };
        let r = Request::from_settings(&s);
        assert_eq!((r.target_fps, r.free_rate), (30, true));
        // The environment may set the variable in the run of the tests; only the key is checked when it does not.
        if std::env::var_os("NXKIT_SET_REFRESH").is_none() {
            assert_eq!(r.set_refresh, Some(120));
        }
        let none = Request::from_settings(&Settings::default());
        assert_eq!(none.target_fps, 60);
    }
}
