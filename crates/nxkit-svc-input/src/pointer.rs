//! The mouse as the host reports it: motion, buttons and wheel notches from the AppKit events of the game window
//! (R63).
//!
//! GameController has no motion that can be read at a call, so the host pushes each event here, as it pushes the
//! touches. The window only sends events while it holds the mouse (relative mode), so a mouse that is free moves no
//! camera. The state has its own lock, so an event on the main thread does not wait for a game thread in the service.

use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use crate::bind::{MouseButtons, mouse};
use crate::look::{Look, LookConfig, WheelPress};

/// The mouse state of player 1.
#[derive(Debug)]
pub(crate) struct Pointer {
    look: Look,
    buttons: u32,
    wheel_up: WheelPress,
    wheel_down: WheelPress,
}

impl Pointer {
    pub(crate) fn new(config: LookConfig) -> Pointer {
        Pointer {
            look: Look::new(config),
            buttons: 0,
            wheel_up: WheelPress::default(),
            wheel_down: WheelPress::default(),
        }
    }

    /// Lets go of every button and stops the camera.
    pub(crate) fn release(&mut self) {
        self.look.clear();
        self.buttons = 0;
        self.wheel_up.clear();
        self.wheel_down.clear();
    }

    /// The buttons and wheel presses at `now`.
    pub(crate) fn buttons(&self, now: Instant) -> MouseButtons {
        MouseButtons {
            buttons: self.buttons,
            wheel_up: self.wheel_up.held(now),
            wheel_down: self.wheel_down.held(now),
        }
    }

    /// The camera stick at `now`.
    pub(crate) fn look(&mut self, now: Instant) -> [f32; 2] {
        self.look.sample(now)
    }
}

static POINTER: Mutex<Option<Pointer>> = Mutex::new(None);

/// Runs `f` on the mouse state, which starts with the default settings.
pub(crate) fn with<R>(f: impl FnOnce(&mut Pointer) -> R) -> R {
    let mut guard = POINTER.lock().unwrap_or_else(PoisonError::into_inner);
    f(guard.get_or_insert_with(|| Pointer::new(LookConfig::default())))
}

/// Starts the mouse state again with new settings.
pub(crate) fn reset(config: LookConfig) {
    let mut guard = POINTER.lock().unwrap_or_else(PoisonError::into_inner);
    *guard = Some(Pointer::new(config));
}

/// The bit of an AppKit button number: 0 left, 1 right, 2 middle, 3 back, 4 forward. Other buttons have none.
fn button_bit(number: u8) -> u32 {
    match number {
        0 => mouse::LEFT,
        1 => mouse::RIGHT,
        2 => mouse::MIDDLE,
        3 => mouse::BACK,
        4 => mouse::FORWARD,
        _ => 0,
    }
}

/// The mouse moved by `dx`, `dy` points, y down.
pub(crate) fn moved(dx: f32, dy: f32) {
    with(|p| p.look.push(dx, dy));
}

/// A button went down or up.
pub(crate) fn button(number: u8, down: bool) {
    let bit = button_bit(number);
    with(|p| {
        if down {
            p.buttons |= bit
        } else {
            p.buttons &= !bit
        }
    });
}

/// The wheel turned by `dy`, positive away from the user.
pub(crate) fn wheel(dy: f32) {
    let now = Instant::now();
    with(|p| {
        if dy > 0.0 {
            p.wheel_up.notch(now);
        } else if dy < 0.0 {
            p.wheel_down.notch(now);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_go_down_and_up() {
        let mut p = Pointer::new(LookConfig::default());
        let now = Instant::now();
        p.buttons |= button_bit(0) | button_bit(2);
        assert_eq!(p.buttons(now).buttons, mouse::LEFT | mouse::MIDDLE);
        p.wheel_down.notch(now);
        assert!(p.buttons(now).wheel_down && !p.buttons(now).wheel_up);
        p.release();
        assert_eq!(p.buttons(now), MouseButtons::default());
    }

    #[test]
    fn button_numbers_follow_appkit() {
        assert_eq!(button_bit(1), mouse::RIGHT);
        assert_eq!(button_bit(3), mouse::BACK);
        assert_eq!(button_bit(4), mouse::FORWARD);
        assert_eq!(button_bit(9), 0);
    }
}
