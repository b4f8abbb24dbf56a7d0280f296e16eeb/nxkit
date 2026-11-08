//! The mouse as the camera stick (R63): mouse motion becomes a right stick deflection that returns to the centre
//! when the mouse stops, and a wheel notch becomes a short button press.
//!
//! The host adds the motion of each mouse event with [`Look::push`]. A read of the npad takes [`Look::sample`], which
//! turns the motion since the last sample into a speed and the speed into a deflection, and follows it with a short
//! smoothing so that a frame without a mouse event does not drop the stick to zero. The time comes in as an argument,
//! so the tests need no clock.

use std::time::{Duration, Instant};

/// The mouse speed in points per second that gives the full deflection at sensitivity 1.
const FULL_SPEED: f32 = 1500.0;
/// The time constant of the smoothing in seconds. The stick reaches 95 % of a new value in three of them.
const SMOOTHING: f32 = 0.03;
/// Samples closer than this keep the last value. A game that reads several npads in one frame gets the same stick.
const MIN_STEP: Duration = Duration::from_millis(4);
/// A pause longer than this counts as this long, so the first motion after a pause is not spread over the pause.
const MAX_STEP: f32 = 0.1;
/// A level below this is the centre. It ends the decay after a stop in about 0.15 s.
const REST: f32 = 0.01;
/// How long a wheel notch holds its button.
const WHEEL_PRESS: Duration = Duration::from_millis(80);
/// How long the button stays up after a wheel press before the next notch counts. A trackpad scroll sends many
/// events, and they would otherwise make one long press.
const WHEEL_GAP: Duration = Duration::from_millis(60);

/// The settings of the mouse camera.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LookConfig {
    /// Scales the mouse speed. 1 gives the full deflection at 1500 points per second. 0.05 to 20.
    pub sensitivity: f32,
    /// Moving the mouse up turns the camera down.
    pub invert_y: bool,
    /// The deflection that the smallest mouse motion gives, 0 to 0.9, so that it gets past the dead zone of the
    /// game.
    pub deadzone: f32,
}

impl Default for LookConfig {
    fn default() -> Self {
        LookConfig {
            sensitivity: 1.0,
            invert_y: false,
            deadzone: 0.12,
        }
    }
}

impl LookConfig {
    /// The settings with every value in its range. A value that is not finite takes the default.
    pub(crate) fn clamped(self) -> LookConfig {
        let d = LookConfig::default();
        let fix =
            |v: f32, lo: f32, hi: f32, def: f32| if v.is_finite() { v.clamp(lo, hi) } else { def };
        LookConfig {
            sensitivity: fix(self.sensitivity, 0.05, 20.0, d.sensitivity),
            invert_y: self.invert_y,
            deadzone: fix(self.deadzone, 0.0, 0.9, d.deadzone),
        }
    }
}

/// The state of the mouse camera.
#[derive(Clone, Debug)]
pub(crate) struct Look {
    config: LookConfig,
    /// The motion in points since the last sample, x right and y down as AppKit reports it.
    pending: [f32; 2],
    /// The smoothed speed as a fraction of the full deflection, x right and y up.
    level: [f32; 2],
    last: Option<Instant>,
}

impl Look {
    pub(crate) fn new(config: LookConfig) -> Look {
        Look {
            config: config.clamped(),
            pending: [0.0; 2],
            level: [0.0; 2],
            last: None,
        }
    }

    /// Adds the motion of one mouse event, in points with y down.
    pub(crate) fn push(&mut self, dx: f32, dy: f32) {
        if dx.is_finite() && dy.is_finite() {
            self.pending[0] += dx;
            self.pending[1] += dy;
        }
    }

    /// Drops the motion and puts the stick to the centre.
    pub(crate) fn clear(&mut self) {
        self.pending = [0.0; 2];
        self.level = [0.0; 2];
    }

    /// The right stick at `now`, x right and y up, each -1 to 1.
    pub(crate) fn sample(&mut self, now: Instant) -> [f32; 2] {
        let Some(last) = self.last else {
            self.last = Some(now);
            self.pending = [0.0; 2];
            return [0.0; 2];
        };
        let step = now.saturating_duration_since(last);
        if step < MIN_STEP {
            return self.stick();
        }
        self.last = Some(now);
        let dt = step.as_secs_f32().min(MAX_STEP);
        let scale = self.config.sensitivity / (FULL_SPEED * dt);
        let flip = if self.config.invert_y { 1.0 } else { -1.0 };
        let target = [self.pending[0] * scale, self.pending[1] * scale * flip];
        self.pending = [0.0; 2];
        let k = 1.0 - (-step.as_secs_f32() / SMOOTHING).exp();
        for (l, t) in self.level.iter_mut().zip(target) {
            *l += (t - *l) * k;
        }
        self.stick()
    }

    /// The level as a stick: limited to the unit circle, tiny values dropped, the rest moved past the dead zone.
    fn stick(&self) -> [f32; 2] {
        let [x, y] = self.level;
        let len = (x * x + y * y).sqrt();
        if len < REST {
            return [0.0; 2];
        }
        let d = self.config.deadzone;
        let out = (d + (1.0 - d) * len.min(1.0)) / len;
        [x * out, y * out]
    }
}

/// The wheel as a button: a notch presses for a short time.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct WheelPress {
    until: Option<Instant>,
}

impl WheelPress {
    /// A notch at `now`. It counts when no press is running and the gap after the last one has passed.
    pub(crate) fn notch(&mut self, now: Instant) {
        if self.until.is_none_or(|u| now >= u + WHEEL_GAP) {
            self.until = Some(now + WHEEL_PRESS);
        }
    }

    /// True while the press runs.
    pub(crate) fn held(&self, now: Instant) -> bool {
        self.until.is_some_and(|u| now < u)
    }

    /// Ends the press.
    pub(crate) fn clear(&mut self) {
        self.until = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: Duration = Duration::from_millis(16);

    fn look(config: LookConfig) -> (Look, Instant) {
        let mut l = Look::new(config);
        let t = Instant::now();
        assert_eq!(l.sample(t), [0.0; 2]);
        (l, t)
    }

    /// Moves the mouse at a steady speed in points per second for `frames` frames and returns the last stick.
    fn steady(l: &mut Look, t: &mut Instant, speed: [f32; 2], frames: u32) -> [f32; 2] {
        let mut s = [0.0; 2];
        for _ in 0..frames {
            l.push(
                speed[0] * FRAME.as_secs_f32(),
                speed[1] * FRAME.as_secs_f32(),
            );
            *t += FRAME;
            s = l.sample(*t);
        }
        s
    }

    #[test]
    fn no_motion_is_the_centre() {
        let (mut l, t) = look(LookConfig::default());
        assert_eq!(l.sample(t + FRAME), [0.0; 2]);
    }

    #[test]
    fn a_steady_motion_settles_on_its_speed() {
        let (mut l, mut t) = look(LookConfig {
            deadzone: 0.0,
            ..LookConfig::default()
        });
        let s = steady(&mut l, &mut t, [750.0, 0.0], 20);
        assert!((s[0] - 0.5).abs() < 0.01, "{s:?}");
        assert_eq!(s[1], 0.0);
    }

    #[test]
    fn mouse_up_is_stick_up_unless_inverted() {
        let (mut l, mut t) = look(LookConfig::default());
        assert!(steady(&mut l, &mut t, [0.0, -300.0], 10)[1] > 0.0);
        let (mut l, mut t) = look(LookConfig {
            invert_y: true,
            ..LookConfig::default()
        });
        assert!(steady(&mut l, &mut t, [0.0, -300.0], 10)[1] < 0.0);
    }

    #[test]
    fn the_stick_returns_to_the_centre_when_the_mouse_stops() {
        let (mut l, mut t) = look(LookConfig::default());
        assert!(steady(&mut l, &mut t, [3000.0, 0.0], 10)[0] > 0.99);
        let s = steady(&mut l, &mut t, [0.0, 0.0], 15);
        assert_eq!(s, [0.0; 2]);
    }

    #[test]
    fn the_stick_stays_in_the_unit_circle() {
        let (mut l, mut t) = look(LookConfig {
            sensitivity: 20.0,
            ..LookConfig::default()
        });
        let [x, y] = steady(&mut l, &mut t, [5000.0, 5000.0], 10);
        assert!((x * x + y * y).sqrt() <= 1.0001);
    }

    #[test]
    fn a_small_motion_gets_past_the_dead_zone() {
        let (mut l, mut t) = look(LookConfig::default());
        let s = steady(&mut l, &mut t, [30.0, 0.0], 10);
        assert!(s[0] >= 0.12 && s[0] < 0.2, "{s:?}");
    }

    #[test]
    fn sensitivity_scales_the_deflection() {
        let (mut a, mut ta) = look(LookConfig {
            deadzone: 0.0,
            ..LookConfig::default()
        });
        let (mut b, mut tb) = look(LookConfig {
            deadzone: 0.0,
            sensitivity: 2.0,
            ..LookConfig::default()
        });
        let sa = steady(&mut a, &mut ta, [300.0, 0.0], 20)[0];
        let sb = steady(&mut b, &mut tb, [300.0, 0.0], 20)[0];
        assert!((sb - 2.0 * sa).abs() < 0.01, "{sa} {sb}");
    }

    #[test]
    fn samples_in_the_same_frame_agree() {
        let (mut l, mut t) = look(LookConfig::default());
        let s = steady(&mut l, &mut t, [600.0, 0.0], 5);
        assert_eq!(l.sample(t + Duration::from_millis(1)), s);
    }

    #[test]
    fn bad_settings_are_limited() {
        let c = LookConfig {
            sensitivity: f32::NAN,
            invert_y: true,
            deadzone: 3.0,
        }
        .clamped();
        assert_eq!(
            c,
            LookConfig {
                sensitivity: 1.0,
                invert_y: true,
                deadzone: 0.9
            }
        );
    }

    #[test]
    fn a_wheel_notch_is_a_short_press_and_a_scroll_does_not_hold_it() {
        let mut w = WheelPress::default();
        let t = Instant::now();
        assert!(!w.held(t));
        w.notch(t);
        assert!(w.held(t + Duration::from_millis(40)));
        w.notch(t + Duration::from_millis(70));
        assert!(!w.held(t + WHEEL_PRESS));
        w.notch(t + WHEEL_PRESS + WHEEL_GAP);
        assert!(w.held(t + WHEEL_PRESS + WHEEL_GAP + Duration::from_millis(1)));
        w.clear();
        assert!(!w.held(t + WHEEL_PRESS + WHEEL_GAP + Duration::from_millis(1)));
    }
}
