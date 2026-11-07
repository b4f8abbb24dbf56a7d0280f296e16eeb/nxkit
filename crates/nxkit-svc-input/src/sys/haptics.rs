//! Core Haptics rumble of one controller, a port of nxnative `haptics_apple.m` (MIT).
//!
//! A controller with a left and a right handle gets one engine for each, driven by motor 0 and motor 1. Any other
//! controller gets one engine that plays both motors mixed. An engine plays a chain of half-second pulses. Their
//! intensity and sharpness follow the game's values through dynamic parameters, so a change takes effect at once.

use std::time::{Duration, Instant};

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_core_haptics::*;
use objc2_foundation::NSArray;
use objc2_game_controller::*;

use crate::rumble::{Level, PlayerLevels};

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
    /// In engine time.
    pulse_end: f64,
    level: Level,
}

/// The rumble state of one controller.
#[derive(Default)]
pub(crate) struct Haptics {
    engines: Vec<Engine>,
    split: bool,
    failures: u32,
    retry_at: Option<Instant>,
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
        CHHapticPattern::initWithEvents_parameters_error(CHHapticPattern::alloc(), &events, &parameters(l)).ok()
    }
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
        self.engines.clear();
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

    /// Creates and starts the engines. False when the controller has no haptics or an engine does not start.
    fn create(&mut self, controller: &GCController) -> bool {
        // SAFETY: plain property reads on a live controller.
        let Some(haptics) = (unsafe { controller.haptics() }) else {
            return false;
        };
        // SAFETY: as above. The localities are strings and the constants live for the whole process.
        let (names, left, right, default) = unsafe {
            (
                haptics.supportedLocalities().to_vec(),
                GCHapticsLocalityLeftHandle,
                GCHapticsLocalityRightHandle,
                GCHapticsLocalityDefault,
            )
        };
        let has = |want: &GCHapticsLocality| names.iter().any(|n| n.to_string() == want.to_string());
        self.split = has(left) && has(right);
        let localities = if self.split { vec![left, right] } else { vec![default] };
        let mut engines = Vec::new();
        for locality in localities {
            // SAFETY: the locality is one of GameController's constants.
            let Some(engine) = (unsafe { haptics.createEngineWithLocality(locality) }) else {
                return false;
            };
            // SAFETY: the engine is alive. Errors come back as `Err`.
            let started = unsafe {
                engine.setPlaysHapticsOnly(true);
                engine.setAutoShutdownEnabled(false);
                engine.startAndReturnError()
            };
            if started.is_err() {
                return false;
            }
            engines.push(Engine { engine, current: None, previous: None, pulse_end: 0.0, level: Level::default() });
        }
        self.engines = engines;
        true
    }

    /// Plays the levels. Engines start with the first level above silence. A failing engine is dropped and tried
    /// again after 1, 2 and 4 seconds, then given up.
    pub(crate) fn update(&mut self, controller: &GCController, levels: &PlayerLevels) {
        let silent = levels.mixed.intensity <= 0.0 && levels.motors.iter().all(|m| m.intensity <= 0.0);
        if self.engines.is_empty() {
            if silent || self.gave_up() || self.retry_at.is_some_and(|t| Instant::now() < t) {
                return;
            }
            if !self.create(controller) {
                self.fail();
                return;
            }
        }
        let wanted = if self.split { levels.motors.to_vec() } else { vec![levels.mixed] };
        let failed = self.engines.iter_mut().zip(wanted).any(|(e, l)| drive(e, l).is_err());
        if failed {
            self.fail();
        } else if !silent {
            self.failures = 0;
        }
    }
}
