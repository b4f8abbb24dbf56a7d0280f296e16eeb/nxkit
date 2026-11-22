//! What the Core Haptics engines of one controller play: a held level for each engine and a short click at the start
//! of a hit. The four-motor translation is a port of the idea of the Xbox mapper of RenHDrum (`src/xbox/mod.rs`,
//! `classify.rs` and `rotor.rs`, <https://github.com/trzz1214/RenHDrum>, MIT OR Apache-2.0, used under MIT). Only its
//! core is taken: the band split of the grips, the start of a hit, the kick that a resting rotor gets and the trigger
//! tick on a strong hit. Its eight kinds of rumble, its strength budget and its game specific cases are not taken.
//!
//! A controller with handles and triggers (an Xbox style pad) plays the low band of both motors on the left handle,
//! which holds the heavy motor, and the high band on the right handle, which holds the light one. The start of a strong
//! hit on the left or right motor of the game ticks the trigger of that side. Any other controller plays as before:
//! motor 0 and motor 1 on the left and right handle, or both mixed on one engine. Every layout gets a click on the
//! engine of a hit when it starts, as strong as the hit and weaker the faster the motor of that engine already turns.

use std::collections::VecDeque;

use crate::rumble::{Level, PlayerRumble, VibrationValue, level};

/// The engines of a controller, in the order of [`Frame`]: the left handle (the only engine of a controller with one),
/// the right handle, the left trigger and the right trigger.
pub(crate) const ENGINES: usize = 4;

/// The haptic engines a controller has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Layout {
    /// One engine for the whole controller.
    One,
    /// A left and a right handle.
    Handles,
    /// Two handles and two triggers.
    Four,
}

impl Layout {
    /// The layout of a controller from the localities it reports.
    pub(crate) fn from_localities(handles: bool, triggers: bool) -> Layout {
        match (handles, triggers) {
            (true, true) => Layout::Four,
            (true, false) => Layout::Handles,
            _ => Layout::One,
        }
    }

    /// The engines that the layout uses, as indices into [`Frame`].
    pub(crate) fn engines(self) -> &'static [usize] {
        match self {
            Layout::One => &[0],
            Layout::Handles => &[0, 1],
            Layout::Four => &[0, 1, 2, 3],
        }
    }
}

/// What the engines play now.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Frame {
    /// The level each engine holds.
    pub(crate) hold: [Level; ENGINES],
    /// A click to play once on an engine.
    pub(crate) click: [Option<Level>; ENGINES],
}

/// Below this a motor is silent (RenHDrum `classify::SILENCE`).
const SILENCE: f32 = 0.01;
/// A hit from silence starts at this level (`classify::KICK_MIN`).
const KICK_MIN: f32 = 0.013;
/// A rise of this much within [`RISE_WINDOW_US`] starts a new hit without silence before it (`classify::ONSET_RISE`).
const ONSET_RISE: f32 = 0.15;
const RISE_WINDOW_US: u64 = 35_000;
/// Hits closer than this are one hit (`REKICK_MIN_GAP_US`).
const REKICK_US: u64 = 80_000;
/// A hit at this level, or one that rose this much, is strong enough for a trigger (`IMPACT_MIN`, `IMPACT_RISE`).
const IMPACT_MIN: f32 = 0.60;
const IMPACT_RISE: f32 = 0.35;
/// A trigger ticks at most once in this time: 20 ms of pulse and 80 ms of rest (`TRIG_PULSE_US`, `TRIG_REST_US`).
const TRIGGER_GAP_US: u64 = 100_000;
/// A trigger tick against the strength of the hit: RenHDrum caps the triggers at 4 % and the grips at 12 %.
const TRIGGER_SHARE: f32 = 0.04 / 0.12;
/// The time constants of the motor of the left and the right handle in microseconds, up and down (`rotor.rs`, the
/// heavy and the light grip). They are estimates for ERM motors, also in RenHDrum.
const TAU_US: [(f32, f32); 2] = [(85_000.0, 150_000.0), (50_000.0, 80_000.0)];

/// One motor of the game: its recent levels.
#[derive(Clone, Debug, Default)]
struct Side {
    level: f32,
    /// The levels of the last [`RISE_WINDOW_US`] with their time.
    recent: VecDeque<(u64, f32)>,
    last_hit: Option<u64>,
    last_trigger: Option<u64>,
}

/// A hit that starts now: its level and whether it is strong enough for a trigger.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Hit {
    level: f32,
    strong: bool,
}

impl Side {
    /// Takes the level of the motor at `now`. Returns the hit that starts with it.
    fn update(&mut self, now: u64, x: f32) -> Option<Hit> {
        while self
            .recent
            .front()
            .is_some_and(|(t, _)| now.saturating_sub(*t) > RISE_WINDOW_US)
        {
            self.recent.pop_front();
        }
        let floor = self
            .recent
            .iter()
            .map(|(_, l)| *l)
            .fold(self.level, f32::min);
        let from_silence = self.level < SILENCE && x >= KICK_MIN;
        let rise = x - floor;
        let starts = (from_silence || rise >= ONSET_RISE)
            && self
                .last_hit
                .is_none_or(|t| now.saturating_sub(t) >= REKICK_US);
        self.level = x;
        if self.recent.len() >= 64 {
            self.recent.pop_front();
        }
        self.recent.push_back((now, x));
        if !starts {
            return None;
        }
        self.last_hit = Some(now);
        Some(Hit {
            level: x,
            strong: x >= IMPACT_MIN || rise >= IMPACT_RISE,
        })
    }

    /// True when the trigger of this side may tick at `now`, and marks it.
    fn trigger(&mut self, now: u64) -> bool {
        if self
            .last_trigger
            .is_some_and(|t| now.saturating_sub(t) < TRIGGER_GAP_US)
        {
            return false;
        }
        self.last_trigger = Some(now);
        true
    }
}

/// The power of a motor, 0 to 1.
fn power(v: &VibrationValue) -> f32 {
    (v.amp_low * v.amp_low + v.amp_high * v.amp_high)
        .sqrt()
        .min(1.0)
}

/// The level of one band of both motors: `high` picks the high band.
fn band(bands: &[VibrationValue; 2], high: bool) -> Level {
    let only = |v: &VibrationValue| VibrationValue {
        amp_low: if high { 0.0 } else { v.amp_low },
        amp_high: if high { v.amp_high } else { 0.0 },
        ..*v
    };
    level(&[only(&bands[0]), only(&bands[1])], 1.0)
}

/// The planner of one controller.
#[derive(Clone, Debug)]
pub(crate) struct Planner {
    layout: Layout,
    sides: [Side; 2],
    /// The estimated speed of the motor of each handle, 0 to 1.
    rotor: [f32; 2],
    drive: [f32; 2],
    last: Option<u64>,
}

impl Planner {
    pub(crate) fn new(layout: Layout) -> Planner {
        Planner {
            layout,
            sides: Default::default(),
            rotor: [0.0; 2],
            drive: [0.0; 2],
            last: None,
        }
    }

    /// Moves the rotor estimates to `now` under the drive since the last call.
    fn spin(&mut self, now: u64) {
        let dt = self.last.map_or(0, |t| now.saturating_sub(t)) as f32;
        self.last = Some(now);
        for i in 0..2 {
            let (up, down) = TAU_US[i];
            let tau = if self.drive[i] >= self.rotor[i] {
                up
            } else {
                down
            };
            let k = 1.0 - (-dt / tau).exp();
            self.rotor[i] = (self.rotor[i] + k * (self.drive[i] - self.rotor[i])).clamp(0.0, 1.0);
        }
    }

    /// The frame for the values of the game at `now`, in microseconds of any steady clock.
    pub(crate) fn plan(&mut self, now: u64, r: &PlayerRumble) -> Frame {
        self.spin(now);
        let mut f = Frame::default();
        match self.layout {
            Layout::One => f.hold[0] = r.levels.mixed,
            Layout::Handles => f.hold[..2].copy_from_slice(&r.levels.motors),
            Layout::Four => {
                f.hold[0] = band(&r.bands, false);
                f.hold[1] = band(&r.bands, true);
            }
        }
        for s in 0..2 {
            let v = &r.bands[s];
            let Some(hit) = self.sides[s].update(now, power(v)) else {
                continue;
            };
            let engine = match self.layout {
                Layout::One => 0,
                Layout::Handles => s,
                Layout::Four => usize::from(v.amp_high > v.amp_low),
            };
            // RenHDrum's `smart_overdrive`: a turning motor needs no extra kick.
            let boost = (1.0 - self.rotor[engine] / hit.level).clamp(0.0, 1.0);
            let sharpness = level(&[*v], 1.0).sharpness;
            let click = |intensity: f32| {
                (intensity >= KICK_MIN).then_some(Level {
                    intensity: intensity.min(1.0),
                    sharpness,
                })
            };
            f.click[engine] = click(hit.level * boost).or(f.click[engine]);
            if self.layout == Layout::Four && hit.strong && self.sides[s].trigger(now) {
                f.click[2 + s] = click(hit.level * TRIGGER_SHARE);
            }
        }
        self.drive = [f.hold[0].intensity, f.hold[1].intensity];
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rumble::Rumble;

    fn rumble(m0: (f32, f32), m1: (f32, f32)) -> PlayerRumble {
        let mut r = Rumble::default();
        let v = |(lo, hi): (f32, f32)| VibrationValue {
            amp_low: lo,
            freq_low: 160.0,
            amp_high: hi,
            freq_high: 320.0,
        };
        r.set(0, 0, v(m0));
        r.set(0, 1, v(m1));
        r.player(0, 1.0, 0.0)
    }

    const MS: u64 = 1000;

    #[test]
    fn the_layout_follows_the_localities() {
        assert_eq!(Layout::from_localities(true, true), Layout::Four);
        assert_eq!(Layout::from_localities(true, false), Layout::Handles);
        assert_eq!(Layout::from_localities(false, true), Layout::One);
        assert_eq!(Layout::Four.engines().len(), ENGINES);
    }

    #[test]
    fn four_motors_split_the_bands_and_the_others_split_the_motors() {
        let r = rumble((0.6, 0.0), (0.0, 0.8));
        let f = Planner::new(Layout::Four).plan(0, &r);
        assert!((f.hold[0].intensity - 0.6).abs() < 1e-5, "low band left");
        assert!((f.hold[1].intensity - 0.8).abs() < 1e-5, "high band right");
        assert_eq!(f.hold[2].intensity + f.hold[3].intensity, 0.0);
        let f = Planner::new(Layout::Handles).plan(0, &rumble((0.6, 0.0), (0.0, 0.0)));
        assert!((f.hold[0].intensity - 0.6).abs() < 1e-5 && f.hold[1].intensity == 0.0);
        let f = Planner::new(Layout::One).plan(0, &r);
        assert_eq!(f.hold[0], r.levels.mixed);
    }

    #[test]
    fn a_hit_from_silence_clicks_once() {
        let mut p = Planner::new(Layout::Handles);
        let quiet = p.plan(0, &rumble((0.0, 0.0), (0.0, 0.0)));
        assert_eq!(quiet.click, [None; ENGINES]);
        let hit = p.plan(5 * MS, &rumble((0.5, 0.0), (0.0, 0.0)));
        let click = hit.click[0].map(|c| c.intensity);
        assert!(click.is_some_and(|c| (c - 0.5).abs() < 1e-5), "{click:?}");
        assert_eq!(hit.click[1], None);
        let held = p.plan(10 * MS, &rumble((0.5, 0.0), (0.0, 0.0)));
        assert_eq!(held.click, [None; ENGINES]);
    }

    #[test]
    fn a_faint_hum_does_not_click() {
        let mut p = Planner::new(Layout::One);
        p.plan(0, &rumble((0.0, 0.0), (0.0, 0.0)));
        let f = p.plan(5 * MS, &rumble((0.005, 0.0), (0.0, 0.0)));
        assert_eq!(f.click, [None; ENGINES]);
    }

    #[test]
    fn hits_closer_than_80_ms_are_one() {
        let mut p = Planner::new(Layout::One);
        let mut clicks = 0;
        for (t, a) in [(0, 0.5), (20, 0.0), (40, 0.5), (60, 0.0), (100, 0.5)] {
            clicks += p
                .plan(t * MS, &rumble((a, 0.0), (0.0, 0.0)))
                .click
                .iter()
                .flatten()
                .count();
        }
        assert_eq!(clicks, 2);
    }

    #[test]
    fn a_turning_motor_gets_a_weaker_click() {
        let mut p = Planner::new(Layout::Handles);
        p.plan(0, &rumble((0.3, 0.0), (0.0, 0.0)));
        for t in 1..40 {
            p.plan(t * 5 * MS, &rumble((0.3, 0.0), (0.0, 0.0)));
        }
        let f = p.plan(200 * MS, &rumble((0.7, 0.0), (0.0, 0.0)));
        let c = f.click[0].map_or(0.0, |c| c.intensity);
        assert!(c > 0.0 && c < 0.7 * 0.7, "{c}");
    }

    #[test]
    fn a_strong_hit_ticks_the_trigger_of_its_side() {
        let mut p = Planner::new(Layout::Four);
        p.plan(0, &rumble((0.0, 0.0), (0.0, 0.0)));
        let f = p.plan(5 * MS, &rumble((0.0, 0.0), (0.9, 0.0)));
        assert_eq!(f.click[2], None);
        let t = f.click[3].map_or(0.0, |c| c.intensity);
        assert!((t - 0.3).abs() < 1e-5, "{t}");
        assert!(
            f.click[0].is_some(),
            "the low band hit kicks the left handle"
        );
        let weak = Planner::new(Layout::Four).plan(0, &rumble((0.2, 0.0), (0.0, 0.0)));
        assert_eq!((weak.click[2], weak.click[3]), (None, None));
        let handles = Planner::new(Layout::Handles).plan(0, &rumble((0.9, 0.0), (0.0, 0.0)));
        assert_eq!((handles.click[2], handles.click[3]), (None, None));
    }

    #[test]
    fn a_trigger_rests_between_ticks() {
        let mut p = Planner::new(Layout::Four);
        let mut ticks = 0;
        for (t, a) in [(0, 0.9), (40, 0.0), (90, 0.9), (130, 0.0), (180, 0.9)] {
            ticks += usize::from(p.plan(t * MS, &rumble((a, 0.0), (0.0, 0.0))).click[2].is_some());
        }
        assert_eq!(ticks, 2, "90 ms is a new hit but too early for the trigger");
    }
}
