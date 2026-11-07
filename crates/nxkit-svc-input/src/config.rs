//! The settings of the input service.

use crate::motion::MotionMap;

/// What the host tells the input service at start.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// True when the console is in handheld mode. Player 1 is then the handheld npad (id 0x20), otherwise the first
    /// full key npad (id 0).
    pub handheld: bool,
    /// Rumble on the controllers that have haptics.
    pub rumble: bool,
    /// Scales the rumble intensity, 0 to 3. 1 plays the game's value as it is.
    pub rumble_gain: f32,
    /// The rumble intensity below which a controller does not move, 0 to 0.9.
    pub rumble_min: f32,
    /// Motion from `GCMotion` into the six-axis sensor.
    pub motion: bool,
    /// The correction of the tilt drift while the controller is near 1 G.
    pub motion_gain: f32,
    /// The axes of the controller as `"x,y,z"` with an optional sign on each, for example `"x,-z,y"`.
    pub motion_axes: String,
    /// True when the acceleration of the platform has the opposite sign, so that a flat controller reads +1 on z.
    pub motion_acc_flip: bool,
    /// Keyboard and mouse through `GCKeyboard` and `GCMouse`.
    pub keyboard: bool,
    /// The pad layout of the title: `simple`, `full` or `touch`. The service keeps it for the controller front end.
    pub pad_layout: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            handheld: false,
            rumble: true,
            rumble_gain: 1.0,
            rumble_min: 0.0,
            motion: true,
            motion_gain: 0.5,
            motion_axes: "x,y,z".to_owned(),
            motion_acc_flip: false,
            keyboard: true,
            pad_layout: "simple".to_owned(),
        }
    }
}

impl Config {
    /// The motion map. A malformed axes string gives the identity.
    pub(crate) fn motion_map(&self) -> MotionMap {
        let map = MotionMap::parse(&self.motion_axes).unwrap_or_default();
        map.with_acc_sign(if self.motion_acc_flip { -1.0 } else { 1.0 })
    }

    /// The gain within 0 to 3. A value that is not finite is 1.
    pub(crate) fn gain(&self) -> f32 {
        if self.rumble_gain.is_finite() { self.rumble_gain.clamp(0.0, 3.0) } else { 1.0 }
    }

    /// The minimum within 0 to 0.9. A value that is not finite is 0.
    pub(crate) fn min(&self) -> f32 {
        if self.rumble_min.is_finite() { self.rumble_min.clamp(0.0, 0.9) } else { 0.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_play_rumble_and_motion() {
        let c = Config::default();
        assert!(c.rumble && c.motion && c.keyboard && !c.handheld);
        assert_eq!(c.motion_map(), MotionMap::default());
    }

    #[test]
    fn gains_are_limited() {
        let c = Config { rumble_gain: 9.0, rumble_min: f32::NAN, ..Config::default() };
        assert_eq!(c.gain(), 3.0);
        assert_eq!(c.min(), 0.0);
    }

    #[test]
    fn a_bad_axes_string_gives_the_identity() {
        let c = Config { motion_axes: "q".to_owned(), ..Config::default() };
        assert_eq!(c.motion_map(), MotionMap::default());
    }
}
