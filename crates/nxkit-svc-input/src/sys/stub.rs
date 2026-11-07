//! The platform layer where there is no GameController: no controller, keyboard or mouse is ever present.

use crate::config::Config;
use crate::keys::KeySet;
use crate::npad::RawPad;
use crate::rumble::PlayerLevels;
use crate::slots::Slots;

/// No devices.
#[derive(Debug, Default)]
pub(crate) struct Platform;

impl Platform {
    pub(crate) fn new(_config: &Config) -> Platform {
        Platform
    }

    pub(crate) fn refresh(&mut self, _slots: &mut Slots) {}

    pub(crate) fn read_pad(&self, _slot: usize) -> Option<RawPad> {
        None
    }

    /// Acceleration in G and rotation rate in rad/s.
    pub(crate) fn read_motion(&self, _slot: usize) -> Option<([f32; 3], [f32; 3])> {
        None
    }

    pub(crate) fn read_keys(&self) -> Option<KeySet> {
        None
    }

    /// The `MouseButton` bits.
    pub(crate) fn read_mouse(&self) -> Option<u32> {
        None
    }

    pub(crate) fn rumble(&mut self, _slot: usize, _levels: &PlayerLevels) {}
}
