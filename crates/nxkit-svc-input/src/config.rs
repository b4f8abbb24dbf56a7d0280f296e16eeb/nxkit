//! The settings of the input service.

use crate::bind::Bindings;
use crate::deadzone::Deadzones;
use crate::input_map::{KeyMap, PadMap};
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
    /// HD Rumble on a Switch Pro Controller: the game's values go to the controller as its own rumble reports. Off,
    /// or when that fails, the controller rumbles through Core Haptics like any other.
    pub hd_rumble: bool,
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
    /// The radial dead zone of the left stick of a controller in percent of its travel, 0 (none) to 50.
    pub stick_deadzone_l: u8,
    /// The radial dead zone of the right stick of a controller, as the left one.
    pub stick_deadzone_r: u8,
    /// Every controller is player 1.
    pub pad_merge: bool,
    /// A new controller becomes a player at its first button press, the lowest player that is free. With
    /// [`Config::pad_merge`] it has no effect.
    pub pad_press_to_join: bool,
    /// What the buttons of a controller send (`NXKIT_PAD_MAP`). The buttons that the host reads itself, Home and the
    /// buttons of the Home menu, are read before it.
    pub pad_map: PadMap,
    /// The functions that `NXKIT_KEYMAP` moves to other keys. It applies on top of the PC layout, and
    /// [`Config::binds`] applies on top of it.
    pub key_map: KeyMap,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            handheld: false,
            rumble: true,
            rumble_gain: 1.0,
            rumble_min: 0.0,
            hd_rumble: true,
            motion: true,
            motion_gain: 0.5,
            motion_axes: "x,y,z".to_owned(),
            motion_acc_flip: false,
            keyboard: true,
            look: LookConfig::default(),
            binds: Vec::new(),
            mouse_capture: true,
            pad_layout: "simple".to_owned(),
            stick_deadzone_l: 0,
            stick_deadzone_r: 0,
            pad_merge: false,
            pad_press_to_join: false,
            pad_map: PadMap::default(),
            key_map: KeyMap::default(),
        }
    }
}

impl Config {
    /// The PC layout with the changes of `key_map`, then those of `binds`.
    pub(crate) fn bindings(&self) -> Bindings {
        let mut b = Bindings::pc();
        self.key_map.apply(&mut b);
        let _ = b.rebind_all(&self.binds);
        b
    }

    /// The motion map. A malformed axes string gives the identity.
    pub(crate) fn motion_map(&self) -> MotionMap {
        let map = MotionMap::parse(&self.motion_axes).unwrap_or_default();
        map.with_acc_sign(if self.motion_acc_flip { -1.0 } else { 1.0 })
    }

    /// The dead zones of the left and the right stick.
    pub(crate) fn deadzones(&self) -> Deadzones {
        Deadzones::new(self.stick_deadzone_l, self.stick_deadzone_r)
    }

    /// Whether a new controller waits for its first button press. Merging every controller into player 1 wins.
    pub(crate) fn press_to_join(&self) -> bool {
        self.pad_press_to_join && !self.pad_merge
    }

    /// The gain within 0 to 3. A value that is not finite is 1.
    pub(crate) fn gain(&self) -> f32 {
        if self.rumble_gain.is_finite() {
            self.rumble_gain.clamp(0.0, 3.0)
        } else {
            1.0
        }
    }

    /// The minimum within 0 to 0.9. A value that is not finite is 0.
    pub(crate) fn min(&self) -> f32 {
        if self.rumble_min.is_finite() {
            self.rumble_min.clamp(0.0, 0.9)
        } else {
            0.0
        }
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
        let c = Config {
            rumble_gain: 9.0,
            rumble_min: f32::NAN,
            ..Config::default()
        };
        assert_eq!(c.gain(), 3.0);
        assert_eq!(c.min(), 0.0);
    }

    #[test]
    fn merging_wins_over_press_to_join() {
        let join = Config {
            pad_press_to_join: true,
            ..Config::default()
        };
        assert!(join.press_to_join());
        let both = Config {
            pad_merge: true,
            ..join
        };
        assert!(!both.press_to_join());
    }

    #[test]
    fn a_bad_axes_string_gives_the_identity() {
        let c = Config {
            motion_axes: "q".to_owned(),
            ..Config::default()
        };
        assert_eq!(c.motion_map(), MotionMap::default());
    }

    #[test]
    fn the_input_file_binds_win_over_the_key_map() {
        use crate::bind::{Action, Source};
        use crate::keys::usage;
        use crate::npad::btn;
        let c = Config {
            key_map: KeyMap::parse("A=40,X=37").unwrap(),
            binds: vec![("k".to_owned(), "zr".to_owned())],
            ..Config::default()
        };
        let b = c.bindings();
        let key = |c| b.get(Source::Key(usage::letter(c)));
        assert_eq!(key(b'k'), Some(Action::Button(btn::ZR)));
        assert_eq!(key(b'l'), Some(Action::Button(btn::X)));
        assert_eq!(key(b'e'), None, "A left E");
    }

    #[test]
    fn binds_change_the_pc_layout() {
        use crate::bind::{Action, Source};
        use crate::keys::usage;
        use crate::npad::btn;
        let c = Config {
            binds: vec![
                ("f".to_owned(), "x".to_owned()),
                ("nope".to_owned(), "a".to_owned()),
            ],
            ..Config::default()
        };
        let b = c.bindings();
        assert_eq!(
            b.get(Source::Key(usage::letter(b'f'))),
            Some(Action::Button(btn::X))
        );
        assert_eq!(
            b.get(Source::Key(usage::letter(b'e'))),
            Some(Action::Button(btn::A))
        );
    }
}
