//! The controller buttons of the host's own screens (the Home menu of `nxkit-host`), apart from the game.
//!
//! The host reads Home, the face buttons, `+`, `-`, the shoulders and the directions of every connected controller by
//! their printed labels, before the layout of the game, and while one of its screens is open the game gets no controller, keyboard or mouse input
//! ([`set_blocked`]). [`claim_home`] asks GameController to give the Home button to the app instead of the system.

use std::sync::atomic::{AtomicBool, Ordering};

/// A stick moved this far (of its range) is a direction, as the d-pad is.
pub(crate) const STICK_DIRECTION: f32 = 0.5;

/// What the host's screens read of the controllers, all of them together.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SystemButtons {
    /// Home, or `-` and `+` held together on a controller without a Home button.
    pub home: bool,
    /// The button labelled A: the bottom one on an Xbox or PlayStation controller, the one printed A on a Nintendo one.
    pub a: bool,
    /// The button labelled B.
    pub b: bool,
    /// The d-pad or the left stick up.
    pub up: bool,
    /// The d-pad or the left stick down.
    pub down: bool,
    /// The d-pad or the left stick left.
    pub left: bool,
    /// The d-pad or the left stick right.
    pub right: bool,
    /// The button labelled X.
    pub x: bool,
    /// The button labelled Y.
    pub y: bool,
    /// `+` (Menu).
    pub plus: bool,
    /// `-` (Options).
    pub minus: bool,
    /// The left shoulder button.
    pub l: bool,
    /// The right shoulder button.
    pub r: bool,
    /// At least one controller is connected.
    pub connected: bool,
}

impl SystemButtons {
    /// Adds the stick position `(x, y)` (y up) to the directions.
    pub(crate) fn add_stick(&mut self, x: f32, y: f32) {
        self.up |= y > STICK_DIRECTION;
        self.down |= y < -STICK_DIRECTION;
        self.left |= x < -STICK_DIRECTION;
        self.right |= x > STICK_DIRECTION;
    }
}

/// True when `-` and `+` together count as Home: a controller without its own Home button.
pub(crate) fn pad_home(home: bool, minus: bool, plus: bool) -> bool {
    home || (minus && plus)
}

// Process-wide (W26): the game's `nn::hid` calls read the controllers through a C function with no Rust context, and
// the host's screens switch them off from the main thread.
static BLOCKED: AtomicBool = AtomicBool::new(false);
// Process-wide (W26): set once by the host before the first lookup of the controllers.
static CLAIM_HOME: AtomicBool = AtomicBool::new(false);

/// While `blocked`, the game reads every controller, the keyboard and the mouse as untouched: a screen of the host
/// has the input.
pub fn set_blocked(blocked: bool) {
    BLOCKED.store(blocked, Ordering::Relaxed);
}

pub(crate) fn blocked() -> bool {
    BLOCKED.load(Ordering::Relaxed)
}

/// Asks GameController for the Home button of every controller that the service takes from now on, so a press
/// reaches the app. The host calls it before the game starts, before the first lookup. macOS 27 respects it when the
/// Home button setting for apps lets the app decide.
pub fn claim_home(on: bool) {
    CLAIM_HOME.store(on, Ordering::Relaxed);
}

pub(crate) fn home_claimed() -> bool {
    CLAIM_HOME.load(Ordering::Relaxed)
}

/// The buttons of the host's screens as they are now. Nothing while no controller is connected.
pub fn system_buttons() -> SystemButtons {
    crate::with(|input| input.system_buttons())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stick_past_half_is_a_direction() {
        let mut b = SystemButtons::default();
        b.add_stick(0.4, -0.6);
        assert!(b.down && !b.up && !b.left && !b.right);
        b.add_stick(-0.9, 0.9);
        assert!(b.up && b.left);
    }

    #[test]
    fn minus_and_plus_together_are_home() {
        assert!(pad_home(true, false, false));
        assert!(pad_home(false, true, true));
        assert!(!pad_home(false, true, false));
    }
}
