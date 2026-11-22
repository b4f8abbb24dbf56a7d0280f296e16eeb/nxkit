//! The refresh rate of the screen that shows the game, and what the display link does about it.
//!
//! `sys/appkit.rs` reads the facts of the screen the window is on and calls [`Refresh::apply`] at the start, when the window
//! moves to another screen and when the display mode changes. `apply` finds the refresh rate of the current mode, plans
//! the ticks of the link per game frame (`nxkit_svc_pacing::refresh`), tells the pacing service and the judder counters,
//! and prints one line when anything changed. [`Refresh::note_measured`] checks the plan against the real tick rate.
//! The design is in `docs/pacing.md`.

use nxkit_svc_pacing::StepMode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use nxkit_nvn::screen::Screen;

use nxkit_metal::{RateRange, display};
use nxkit_svc_pacing::refresh::{self, TickPlan};

use crate::settings::Settings;

/// The lowest rate under variable refresh when `NXKIT_VRR_MIN` does not say: the game steps by the measured time, but its
/// physics and its audio want at least the rate of the Switch.
const DEFAULT_VRR_FLOOR: u32 = 60;

/// What the user and the game ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    /// The frame rate the game aims at.
    pub target_fps: u32,
    /// The rate of the game's vsync: 60, or 120 when the game runs at 120 fps through its engine's frame limit. The hold
    /// of the frame step counts in these vsyncs.
    pub vsync_hz: u32,
    /// Whether any frame rate is fine for the game.
    pub free_rate: bool,
    /// The refresh rate to switch the display to, when set.
    pub set_refresh: Option<u32>,
    /// Timed presents: `Some(true)` forces them, `Some(false)` forbids them, `None` uses them on a screen that adapts its rate.
    pub timed: Option<bool>,
    /// Variable refresh: `false` when `NXKIT_VRR=0` turns it off.
    pub vrr: bool,
    /// The lowest rate the game needs under variable refresh. `NXKIT_VRR_MIN`, 60 by default.
    pub vrr_floor: u32,
}

/// How frames reach the screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Presentation {
    /// The display link ticks at a fixed rate, and a frame shows on a tick of that grid.
    Grid,
    /// The display link has a range of rates, a frame shows as soon as it is ready and the screen follows.
    Vrr(RateRange),
}

/// Variable refresh when the screen adapts its rate and the game steps by the measured frame time (`StepMode::Variable`,
/// `variable_target` is its target). The fixed grid otherwise. `max_fps` is the highest rate of the screen. A screen
/// that cannot go above the floor has nothing to follow, so it keeps the grid.
pub fn choose_presentation(
    request: &Request,
    adaptive: bool,
    variable_target: Option<u32>,
    max_fps: u32,
) -> Presentation {
    let Some(target) = variable_target else {
        return Presentation::Grid;
    };
    if !request.vrr || !adaptive || max_fps <= request.vrr_floor {
        return Presentation::Grid;
    }
    Presentation::Vrr(RateRange::plan(request.vrr_floor, max_fps, target))
}

impl Request {
    /// The request of a game that runs at 120 fps through its engine's frame limit (`seams::runs_at_120`): the link
    /// and the vsync at 120, so a hold of one tick of 1/120 s shows each frame once.
    pub fn at_120(self, high: bool) -> Self {
        if high {
            Self {
                target_fps: 120,
                vsync_hz: 120,
                ..self
            }
        } else {
            self
        }
    }

    /// The request of `settings` and the environment. `NXKIT_SET_REFRESH` and `NXKIT_TIMED_PRESENT` win over the keys.
    pub fn from_settings(settings: &Settings, env: &crate::hostenv::HostEnv) -> Self {
        let env_refresh = env.set_refresh;
        let set_refresh = env_refresh
            .or((settings.set_refresh > 0).then_some(settings.set_refresh))
            .filter(|hz| *hz > 0);
        let timed = env.timed_present;
        let vrr = env.vrr;
        let vrr_floor = env.vrr_min.unwrap_or(DEFAULT_VRR_FLOOR);
        Self {
            target_fps: if settings.fps_target == 0 {
                60
            } else {
                settings.fps_target.min(60)
            },
            // A Switch game steps in vsyncs of 1/60 s.
            vsync_hz: 60,
            free_rate: settings.fps_free,
            set_refresh,
            timed,
            vrr,
            vrr_floor,
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
    /// The target of the variable frame step, when the game asks to step that way.
    variable_target: Option<u32>,
    /// The step the game takes on the fixed grid instead of the variable step.
    grid_step: Option<StepMode>,
    /// Whether the pacing service steps by the variable step now, once [`apply`] decided it.
    stepping_variable: Option<bool>,
    presentation: Presentation,
    plan: Option<TickPlan>,
    announced: String,
    /// The display ids that a switch was tried for with this rate, so that a failed switch does not repeat.
    tried: Vec<(u32, u32)>,
    /// The measured rates already reported, rounded to Hz.
    reported: u32,
}

/// The refresh planning of one run. The host makes it once before the services start and passes it to the pacing seam,
/// the window and the tick hook (W26).
pub struct Refresh {
    state: Mutex<State>,
    /// The number of [`Refresh::apply`] calls. The tick hook measures anew when it changes.
    generation: AtomicU64,
    /// What the screen showed of the presents: the refresh tells it the rate of the screen.
    screen: Arc<Screen>,
    /// The minimum stay of the presents: the refresh sets it, the present blit of the GPU backend reads it.
    timed: Arc<nxkit_metal::TimedPresent>,
}

impl Refresh {
    /// Plans for `request` and tells `screen` the rates.
    pub fn new(request: Request, screen: Arc<Screen>) -> Self {
        Refresh {
            state: Mutex::new(State {
                request,
                variable_target: None,
                grid_step: None,
                stepping_variable: None,
                presentation: Presentation::Grid,
                plan: None,
                announced: String::new(),
                tried: Vec::new(),
                reported: 0,
            }),
            generation: AtomicU64::new(0),
            screen,
            timed: Arc::default(),
        }
    }

    /// The minimum stay of the presents, for the GPU backend.
    pub fn timed_present(&self) -> Arc<nxkit_metal::TimedPresent> {
        self.timed.clone()
    }

    /// The number of plans made so far.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Tells the module that the game asks to step by the measured frame time (`StepMode::Variable`) with this target, or
    /// does not. `grid_step` is the step it takes when the frames go on the fixed grid. The variable step only works when
    /// each frame shows as soon as it is ready: on the grid it left the load-driven pacer out, and Ocarina of Time ran at
    /// about 10 fps on a fixed 120 Hz screen (2026-11-09). So [`Refresh::apply`] switches the pacing service to the variable step
    /// under variable refresh and to `grid_step` otherwise. The pacing seam calls it before the window opens.
    pub fn set_variable_step(&self, target: Option<u32>, grid_step: StepMode) {
        let mut st = self.state();
        st.variable_target = target;
        st.grid_step = Some(grid_step);
        st.stepping_variable = None;
    }

    /// How frames reach the screen, from the latest [`Refresh::apply`].
    pub fn presentation(&self) -> Presentation {
        self.state().presentation
    }

    /// The rate range of the display link under variable refresh, `None` for the fixed grid.
    pub fn vrr_range(&self) -> Option<RateRange> {
        match self.presentation() {
            Presentation::Vrr(r) => Some(r),
            Presentation::Grid => None,
        }
    }

    /// The plan of the latest [`Refresh::apply`].
    pub fn plan(&self) -> Option<TickPlan> {
        self.state().plan
    }
}

/// The refresh rate of the current mode of the screen, and where it came from.
fn refresh_of(facts: &ScreenFacts) -> (f64, &'static str) {
    if let Some(hz) = facts.display.and_then(display::current_refresh_hz) {
        return (hz, "CoreGraphics mode");
    }
    (
        f64::from(facts.max_fps.max(1)),
        if facts.adaptive {
            "adaptive, highest rate"
        } else {
            "NSScreen"
        },
    )
}

impl Refresh {
    /// Plans for `facts`. It switches the display when `NXKIT_SET_REFRESH` asks, tells the pacing service the rates, turns
    /// timed presents on or off and prints one line when the screen, the rate or the plan changed. Returns the plan.
    pub fn apply(&self, facts: &ScreenFacts) -> TickPlan {
        let mut st = self.state();
        let req = st.request;
        let mut note = String::new();
        if let (Some(want), Some(id)) = (req.set_refresh, facts.display)
            && !facts.adaptive
            && !st.tried.contains(&(id, want))
        {
            st.tried.push((id, want));
            match display::switch_refresh(id, f64::from(want)) {
                Ok(Some(sw)) => {
                    note = format!(
                        "; switched the display from {:.2} Hz to {:.2} Hz, restored at exit",
                        sw.from_hz, sw.to_hz
                    );
                    display::keep_active(sw);
                }
                Ok(None) => note = format!("; the display shows {want} Hz already"),
                Err(e) => eprintln!("nxkit-host: NXKIT_SET_REFRESH={want}: {e}; the refresh stays"),
            }
        }
        let (hz, source) = refresh_of(facts);
        let plan = refresh::plan_ticks(hz, req.target_fps, req.free_rate, facts.adaptive);
        let presentation =
            choose_presentation(&req, facts.adaptive, st.variable_target, facts.max_fps);
        let vrr = match presentation {
            Presentation::Vrr(r) => Some(r),
            Presentation::Grid => None,
        };
        let fps = plan.fps as f32;
        // The service takes the link rate and the vsync rate of the game, so that a frame step of N vsyncs holds N * ticks.
        // Under variable refresh the link runs between the floor and the highest rate. The preferred rate stands for
        // it.
        let link_hz = vrr.map_or(plan.link_hz as f32, |r| r.preferred);
        let _ = nxkit_svc_pacing::set_rates(
            link_hz,
            if plan.fps == req.target_fps {
                req.vsync_hz as f32
            } else {
                fps
            },
        );
        self.screen.set_mode_hz(hz.round() as u32);
        let refresh_hz = if let Some(r) = vrr {
            f64::from(r.preferred)
        } else if facts.adaptive {
            f64::from(plan.link_hz)
        } else {
            hz
        };
        self.screen.set_refresh_hz(refresh_hz);
        // The game's time follows the frames the display shows, not the starts of frames.
        nxkit_svc_pacing::set_step_by_shown(vrr.is_some());
        if let (Some(target), Some(grid)) = (st.variable_target, st.grid_step)
            && st.stepping_variable != Some(vrr.is_some())
        {
            let mode = if vrr.is_some() {
                StepMode::Variable(target)
            } else {
                grid
            };
            match nxkit_svc_pacing::set_step_mode(mode) {
                Ok(()) => st.stepping_variable = Some(vrr.is_some()),
                Err(e) => eprintln!("nxkit-host: the frame step stays: {e:?}"),
            }
        }
        // Off unless asked for (NXKIT_TIMED_PRESENT=1): on a ProMotion screen a minimum duration per present let macOS
        // lower the panel's rate, and a frame of 0.8 vsyncs of work waited about 3.8 vsyncs for the screen (Ocarina of
        // Time, 2026-11-09). Without it each frame shows on the next tick of the link.
        // Under variable refresh a frame shows as soon as it is ready, so a minimum duration would only hold it back.
        let timed = req.timed.unwrap_or(false) && vrr.is_none();
        self.timed
            .set(timed.then(|| 1.0 / f64::from(plan.fps.max(1))));
        let mut hint = String::new();
        if !plan.exact {
            let choices = facts
                .display
                .map(display::refresh_choices)
                .unwrap_or_default();
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
            "nxkit-host: screen \"{}\" refresh {hz:.2} Hz ({source}{}), link {} Hz, {} tick{} per {} fps frame{}{}{}{}{note}{hint}",
            facts.name,
            if facts.adaptive { ", adaptive" } else { "" },
            link_text(vrr, plan.link_hz),
            plan.ticks_per_frame,
            if plan.ticks_per_frame == 1 { "" } else { "s" },
            plan.fps,
            if plan.exact { "" } else { " (uneven)" },
            if timed { ", timed presents" } else { "" },
            vrr_text(vrr),
            if req.free_rate && plan.fps != req.target_fps {
                ", free frame rate"
            } else {
                ""
            },
        );
        if line != st.announced {
            eprintln!("{line}");
            st.announced = line;
        }
        st.plan = Some(plan);
        st.presentation = presentation;
        st.reported = 0;
        self.generation.fetch_add(1, Ordering::Relaxed);
        plan
    }

    /// The link ticked at `measured_hz` over the last second while the plan asked for `plan.link_hz`. When they differ by
    /// more than 4%, the service gets the measured rate, so that a hold counts the ticks that really come. Returns the rate
    /// when it changed the service.
    pub fn note_measured(&self, measured_hz: f64) -> Option<f64> {
        let mut st = self.state();
        let plan = st.plan?;
        // Under variable refresh the tick rate follows the frames and is no fixed rate to check the plan against.
        if matches!(st.presentation, Presentation::Vrr(_)) {
            return None;
        }
        let rounded = measured_hz.round() as u32;
        if !refresh::rate_differs(measured_hz, f64::from(plan.link_hz))
            || rounded == st.reported
            || rounded == 0
        {
            return None;
        }
        st.reported = rounded;
        let req = st.request;
        let vsync = if plan.fps == req.target_fps {
            req.vsync_hz as f32
        } else {
            plan.fps as f32
        };
        let _ = nxkit_svc_pacing::set_rates(rounded as f32, vsync);
        eprintln!(
            "nxkit-host: the display link ticks at {measured_hz:.1} Hz, not the {} Hz that was asked for, so a frame holds {:.1} ticks per vsync now",
            plan.link_hz,
            measured_hz / f64::from(vsync)
        );
        Some(measured_hz)
    }
}

/// The link rate of the screen line: one rate on the grid, the range under variable refresh.
fn link_text(vrr: Option<RateRange>, link_hz: u32) -> String {
    vrr.map_or_else(|| link_hz.to_string(), |r| format!("{}-{}", r.min, r.max))
}

/// The part of the screen line that says variable refresh is on, with the range of the link.
fn vrr_text(vrr: Option<RateRange>) -> String {
    vrr.map_or_else(String::new, |r| {
        format!(
            ", VRR on (frames show when ready, link {}-{} Hz, prefers {})",
            r.min, r.max, r.preferred
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> Request {
        Request {
            target_fps: 60,
            vsync_hz: 60,
            free_rate: false,
            set_refresh: None,
            timed: None,
            vrr: true,
            vrr_floor: 60,
        }
    }

    #[test]
    fn an_adaptive_screen_with_a_variable_step_gets_a_range() {
        let p = choose_presentation(&request(), true, Some(120), 120);
        let Presentation::Vrr(r) = p else {
            panic!("{p:?}");
        };
        assert_eq!((r.min, r.max, r.preferred), (60.0, 120.0, 120.0));
        // An external Adaptive-Sync screen of 144 Hz and a game that aims at 240.
        let Presentation::Vrr(r) = choose_presentation(&request(), true, Some(240), 144) else {
            panic!("no range");
        };
        assert_eq!((r.min, r.max, r.preferred), (60.0, 144.0, 144.0));
        assert_eq!(vrr_text(None), "");
        assert!(vrr_text(Some(r)).contains("VRR on"));
        assert_eq!(link_text(Some(r), 60), "60-144");
        assert_eq!(link_text(None, 120), "120");
    }

    #[test]
    fn everything_else_keeps_the_grid() {
        let r = request();
        // The hold-based step (no variable target) and a screen that does not adapt.
        assert_eq!(choose_presentation(&r, true, None, 120), Presentation::Grid);
        assert_eq!(
            choose_presentation(&r, false, Some(120), 120),
            Presentation::Grid
        );
        // A screen that cannot go above the floor has nothing to follow.
        assert_eq!(
            choose_presentation(&r, true, Some(120), 60),
            Presentation::Grid
        );
        // NXKIT_VRR=0.
        let off = Request { vrr: false, ..r };
        assert_eq!(
            choose_presentation(&off, true, Some(120), 120),
            Presentation::Grid
        );
        // A higher floor from NXKIT_VRR_MIN.
        let Presentation::Vrr(v) =
            choose_presentation(&Request { vrr_floor: 80, ..r }, true, Some(120), 120)
        else {
            panic!("no range");
        };
        assert_eq!(v.min, 80.0);
    }

    #[test]
    fn the_request_takes_the_target_from_the_settings() {
        let s = Settings {
            fps_target: 30,
            fps_free: true,
            set_refresh: 120,
            ..Settings::default()
        };
        let env = crate::hostenv::HostEnv::default();
        let r = Request::from_settings(&s, &env);
        assert_eq!((r.target_fps, r.free_rate), (30, true));
        assert_eq!(r.set_refresh, Some(120));
        let set = crate::hostenv::HostEnv::from_env(&nxkit_abi::env::Env::from_pairs([(
            "NXKIT_SET_REFRESH",
            "60",
        )]));
        assert_eq!(Request::from_settings(&s, &set).set_refresh, Some(60));
        let none = Request::from_settings(&Settings::default(), &env);
        assert_eq!(none.target_fps, 60);
    }
}
