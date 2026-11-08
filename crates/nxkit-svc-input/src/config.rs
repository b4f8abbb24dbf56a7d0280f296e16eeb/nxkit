//! The settings of the input service.

use crate::bind::Bindings;
use crate::look::LookConfig;
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
    /// Keyboard and mouse: the keys through `GCKeyboard`, the mouse through the events of the host window (R63).
    pub keyboard: bool,
    /// The mouse camera: sensitivity, inverted y and the dead zone of the game.
    pub look: LookConfig,
    /// Changes to the PC layout, as `(key, action)` pairs in the order of the input file, for example `("f", "rstick")`
    /// or `("mouse_left", "none")`. The names are those of `bind.rs`. A pair that does not parse is skipped, see
    /// [`crate::bind_errors`].
    pub binds: Vec<(String, String)>,
    /// The window holds and hides the mouse while it has the focus and no controller is connected.
    pub mouse_capture: bool,
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
            look: LookConfig::default(),
            binds: Vec::new(),
            mouse_capture: true,
            pad_layout: "simple".to_owned(),
        }
    }
}

impl Config {
    /// The PC layout with the changes of `binds`.
    pub(crate) fn bindings(&self) -> Bindings {
        let mut b = Bindings::pc();
        let _ = b.rebind_all(&self.binds);
        b
    }

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
        assert!(c.rumble && c.motion && c.keyboard && c.mouse_capture && !c.handheld);
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

    #[test]
    fn binds_change_the_pc_layout() {
        use crate::bind::{Action, Source};
        use crate::keys::usage;
        use crate::npad::btn;
        let c = Config {
            binds: vec![("f".to_owned(), "x".to_owned()), ("nope".to_owned(), "a".to_owned())],
            ..Config::default()
        };
        let b = c.bindings();
        assert_eq!(b.get(Source::Key(usage::letter(b'f'))), Some(Action::Button(btn::X)));
        assert_eq!(b.get(Source::Key(usage::letter(b'e'))), Some(Action::Button(btn::A)));
    }
}
