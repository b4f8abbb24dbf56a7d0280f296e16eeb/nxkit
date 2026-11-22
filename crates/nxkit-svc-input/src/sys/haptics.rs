//! Core Haptics rumble of one controller, a port of the engine `haptics_apple.m` (MIT).
//!
//! The controller gets one engine for each locality that `haptic_plan` uses: the left and the right handle, and the
//! left and the right trigger where it has them, or one engine for the whole controller. An engine holds its level
//! through a chain of half-second pulses. Their intensity and sharpness follow the game's values through dynamic
//! parameters, so a change takes effect at once. A click of the plan is a transient event on its engine.

use std::time::{Duration, Instant};

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_core_haptics::*;
use objc2_foundation::NSArray;
use objc2_game_controller::*;

use crate::haptic_plan::{ENGINES, Layout, Planner};
use crate::rumble::{Level, PlayerRumble};

/// The length of one pulse in seconds.
const PULSE: f64 = 0.5;
/// A new pulse starts when the running one has less than this left.
const RENEW: f64 = 0.15;
/// After this many failures in a row the controller gets no more engines.
const MAX_FAILURES: u32 = 4;

type Player = Retained<ProtocolObject<dyn CHHapticPatternPlayer>>;

struct Engine {
    engine: Retained<CHHapticEngine>,
    current: Option<Player>,
    previous: Option<Player>,
    /// The player of the last click, kept until the next one.
    click: Option<Player>,
    /// In engine time.
    pulse_end: f64,
    level: Level,
}

/// The rumble state of one controller.
pub(crate) struct Haptics {
    engines: [Option<Engine>; ENGINES],
    planner: Planner,
    failures: u32,
    retry_at: Option<Instant>,
    logged: bool,
    epoch: Instant,
}

impl Default for Haptics {
    fn default() -> Self {
        Haptics {
            engines: Default::default(),
            planner: Planner::new(Layout::One),
            failures: 0,
            retry_at: None,
            logged: false,
            epoch: Instant::now(),
        }
    }
}

/// The haptic localities of a controller by name, empty when it has no haptics.
pub(crate) fn localities(controller: &GCController) -> Vec<String> {
    // SAFETY: plain property reads on a live controller and its haptics.
    unsafe { controller.haptics() }
        // SAFETY: as above.
        .map(|h| unsafe { h.supportedLocalities() }.to_vec())
        .unwrap_or_default()
        .iter()
        .map(|n| n.to_string())
        .collect()
}

/// The layout of a controller with these localities and the locality of each engine of it.
fn layout(names: &[String]) -> (Layout, [&'static GCHapticsLocality; ENGINES]) {
    // SAFETY: the localities are constants of GameController that live for the whole process.
    let (default, lh, rh, lt, rt) = unsafe {
        (
            GCHapticsLocalityDefault,
            GCHapticsLocalityLeftHandle,
            GCHapticsLocalityRightHandle,
            GCHapticsLocalityLeftTrigger,
            GCHapticsLocalityRightTrigger,
        )
    };
    let has = |want: &GCHapticsLocality| names.iter().any(|n| *n == want.to_string());
    let layout = Layout::from_localities(has(lh) && has(rh), has(lt) && has(rt));
    let first = if layout == Layout::One { default } else { lh };
    (layout, [first, rh, lt, rt])
}

/// A line for the log and the probe: the localities of a controller and what the plan does with them.
pub(crate) fn describe(names: &[String]) -> String {
    if names.is_empty() {
        return "no haptics".to_owned();
    }
    let plays = match layout(names).0 {
        Layout::One => "one engine, both motors mixed",
        Layout::Handles => "motor 0 on the left handle, motor 1 on the right handle",
        Layout::Four => {
            "low band on the left handle, high band on the right handle, hits on the triggers"
        }
    };
    format!("haptic localities {}: {plays}", names.join(", "))
}

/// The dynamic parameters for a level: the intensity as a factor and the sharpness as an offset from 0.5.
fn parameters(l: Level) -> Retained<NSArray<CHHapticDynamicParameter>> {
    // SAFETY: the initialisers take an allocated object, a parameter id string that lives for the whole process, a
    // value and a time. The ids are read from Core Haptics' constants.
    unsafe {
        let intensity = CHHapticDynamicParameter::initWithParameterID_value_relativeTime(
            CHHapticDynamicParameter::alloc(),
            CHHapticDynamicParameterIDHapticIntensityControl,
            l.intensity,
            0.0,
        );
        let sharpness = CHHapticDynamicParameter::initWithParameterID_value_relativeTime(
            CHHapticDynamicParameter::alloc(),
            CHHapticDynamicParameterIDHapticSharpnessControl,
            l.sharpness - 0.5,
            0.0,
        );
        NSArray::from_retained_slice(&[intensity, sharpness])
    }
}

/// One continuous pulse at full intensity and mid sharpness, scaled by the dynamic parameters of `l`.
fn pattern(l: Level) -> Option<Retained<CHHapticPattern>> {
    // SAFETY: as in `parameters`. The arrays hold objects of the element type.
    unsafe {
        let intensity = CHHapticEventParameter::initWithParameterID_value(
            CHHapticEventParameter::alloc(),
            CHHapticEventParameterIDHapticIntensity,
            1.0,
        );
        let sharpness = CHHapticEventParameter::initWithParameterID_value(
            CHHapticEventParameter::alloc(),
            CHHapticEventParameterIDHapticSharpness,
            0.5,
        );
        let event_params = NSArray::from_retained_slice(&[intensity, sharpness]);
        let event = CHHapticEvent::initWithEventType_parameters_relativeTime_duration(
            CHHapticEvent::alloc(),
            CHHapticEventTypeHapticContinuous,
            &event_params,
            0.0,
            PULSE,
        );
        let events = NSArray::from_retained_slice(&[event]);
        CHHapticPattern::initWithEvents_parameters_error(
            CHHapticPattern::alloc(),
            &events,
            &parameters(l),
        )
        .ok()
    }
}

/// One transient event at the intensity and sharpness of `l`.
fn click_pattern(l: Level) -> Option<Retained<CHHapticPattern>> {
    // SAFETY: as in `pattern`.
    unsafe {
        let intensity = CHHapticEventParameter::initWithParameterID_value(
            CHHapticEventParameter::alloc(),
            CHHapticEventParameterIDHapticIntensity,
            l.intensity,
        );
        let sharpness = CHHapticEventParameter::initWithParameterID_value(
            CHHapticEventParameter::alloc(),
            CHHapticEventParameterIDHapticSharpness,
            l.sharpness,
        );
        let event_params = NSArray::from_retained_slice(&[intensity, sharpness]);
        let event = CHHapticEvent::initWithEventType_parameters_relativeTime(
            CHHapticEvent::alloc(),
            CHHapticEventTypeHapticTransient,
            &event_params,
            0.0,
        );
        let events = NSArray::from_retained_slice(&[event]);
        CHHapticPattern::initWithEvents_parameters_error(
            CHHapticPattern::alloc(),
            &events,
            &NSArray::new(),
        )
        .ok()
    }
}

/// Plays a click on an engine now.
fn click(e: &mut Engine, l: Level) -> Result<(), ()> {
    let pattern = click_pattern(l).ok_or(())?;
    // SAFETY: the pattern is a valid `CHHapticPattern` and the engine is started.
    let player = unsafe { e.engine.createPlayerWithPattern_error(&pattern) }.map_err(|_| ())?;
    // SAFETY: the player was just created. Time 0 is immediate.
    unsafe { player.startAtTime_error(0.0) }.map_err(|_| ())?;
    e.click = Some(player);
    Ok(())
}

/// Sends the level to the running and the previous pulse, at once.
fn send(e: &Engine, l: Level) -> Result<(), ()> {
    let params = parameters(l);
    for p in [&e.previous, &e.current].into_iter().flatten() {
        // SAFETY: the player is alive and `params` holds dynamic parameters. Time 0 is immediate.
        unsafe { p.sendParameters_atTime_error(&params, 0.0) }.map_err(|_| ())?;
    }
    Ok(())
}

/// Brings one engine to the level `l`.
fn drive(e: &mut Engine, l: Level) -> Result<(), ()> {
    if l.intensity <= 0.0 {
        if e.level.intensity > 0.0 {
            send(e, l)?;
        }
        e.level = l;
        return Ok(());
    }
    // SAFETY: `currentTime` takes no arguments.
    let now = unsafe { e.engine.currentTime() };
    if e.current.is_none() || e.pulse_end - now < RENEW {
        let pattern = pattern(l).ok_or(())?;
        // SAFETY: the pattern is a valid `CHHapticPattern` and the engine is started.
        let player = unsafe { e.engine.createPlayerWithPattern_error(&pattern) }.map_err(|_| ())?;
        let at = now.max(e.pulse_end);
        // SAFETY: the player was just created.
        unsafe { player.startAtTime_error(at) }.map_err(|_| ())?;
        e.pulse_end = at + PULSE;
        e.previous = e.current.replace(player);
        if e.level != l {
            send(e, l)?;
        }
    } else if e.level != l {
        send(e, l)?;
    }
    e.level = l;
    Ok(())
}

impl Haptics {
    fn fail(&mut self) {
        self.engines = Default::default();
        self.failures += 1;
        self.retry_at = if self.failures >= MAX_FAILURES {
            None
        } else {
            Some(Instant::now() + Duration::from_secs(1 << (self.failures - 1)))
        };
    }

    fn gave_up(&self) -> bool {
        self.failures >= MAX_FAILURES
    }

    /// Creates and starts the engines. False when the controller has no haptics or an engine does not start. The
    /// first creation logs the localities of the controller.
    fn create(&mut self, controller: &GCController) -> bool {
        // SAFETY: plain property reads on a live controller.
        let Some(haptics) = (unsafe { controller.haptics() }) else {
            return false;
        };
        let names = localities(controller);
        if !self.logged {
            self.logged = true;
            // SAFETY: a property read on a live controller.
            let name =
                unsafe { controller.vendorName() }.map_or_else(String::new, |n| n.to_string());
            eprintln!("nxkit-input: rumble: {name}: {}", describe(&names));
        }
        let (layout, wanted) = layout(&names);
        let mut engines: [Option<Engine>; ENGINES] = Default::default();
        for &i in layout.engines() {
            // SAFETY: the locality is one of GameController's constants.
            let Some(engine) = (unsafe { haptics.createEngineWithLocality(wanted[i]) }) else {
                return false;
            };
            // SAFETY: The engine is alive. Errors come back as `Err`.
            let started = unsafe {
                engine.setPlaysHapticsOnly(true);
                engine.setAutoShutdownEnabled(false);
                engine.startAndReturnError()
            };
            if started.is_err() {
                return false;
            }
            engines[i] = Some(Engine {
                engine,
                current: None,
                previous: None,
                click: None,
                pulse_end: 0.0,
                level: Level::default(),
            });
        }
        self.engines = engines;
        self.planner = Planner::new(layout);
        true
    }

    /// Plays the rumble of a player. Engines start with the first level above silence. A failing engine drops all of
    /// them, and they are tried again after 1, 2 and 4 seconds, then given up.
    pub(crate) fn update(&mut self, controller: &GCController, rumble: &PlayerRumble) {
        let levels = &rumble.levels;
        let silent =
            levels.mixed.intensity <= 0.0 && levels.motors.iter().all(|m| m.intensity <= 0.0);
        if self.engines.iter().all(Option::is_none) {
            if silent || self.gave_up() || self.retry_at.is_some_and(|t| Instant::now() < t) {
                return;
            }
            if !self.create(controller) {
                self.fail();
                return;
            }
        }
        let now = self.epoch.elapsed().as_micros() as u64;
        let frame = self.planner.plan(now, rumble);
        let mut failed = false;
        for (i, e) in self.engines.iter_mut().enumerate() {
            let Some(e) = e else { continue };
            failed |= drive(e, frame.hold[i]).is_err();
            if let Some(c) = frame.click[i] {
                failed |= click(e, c).is_err();
            }
        }
        if failed {
            self.fail();
        } else if !silent {
            self.failures = 0;
        }
    }
}
