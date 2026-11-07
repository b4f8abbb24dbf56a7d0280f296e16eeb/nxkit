//! Keyboard keys as USB HID usage ids, the nxnative key map for player 1 and the `KeyboardState` layout.
//!
//! The usage ids are those of the USB HID usage table (page 7), which `nn::hid::KeyboardKey` also uses as bit
//! numbers. The platform layer reads `GCKeyboard` by key code and sets the bit of the matching usage here, so no
//! number of a `GCKeyCode` is assumed.

use crate::npad::{RawKeys, btn};

/// Usage ids that the map and the platform layer use.
pub(crate) mod usage {
    /// `a` is 0x04, so a letter is `A + (c - 'a')`.
    pub(crate) const A: u8 = 0x04;
    pub(crate) const RETURN: u8 = 0x28;
    pub(crate) const ESCAPE: u8 = 0x29;
    pub(crate) const BACKSPACE: u8 = 0x2a;
    pub(crate) const TAB: u8 = 0x2b;
    pub(crate) const SPACE: u8 = 0x2c;
    pub(crate) const F1: u8 = 0x3a;
    pub(crate) const DELETE_FORWARD: u8 = 0x4c;
    pub(crate) const RIGHT: u8 = 0x4f;
    pub(crate) const LEFT: u8 = 0x50;
    pub(crate) const DOWN: u8 = 0x51;
    pub(crate) const UP: u8 = 0x52;
    pub(crate) const LEFT_CONTROL: u8 = 0xe0;
    pub(crate) const LEFT_SHIFT: u8 = 0xe1;
    pub(crate) const LEFT_ALT: u8 = 0xe2;
    pub(crate) const LEFT_GUI: u8 = 0xe3;
    pub(crate) const RIGHT_CONTROL: u8 = 0xe4;
    pub(crate) const RIGHT_SHIFT: u8 = 0xe5;
    pub(crate) const RIGHT_ALT: u8 = 0xe6;
    pub(crate) const RIGHT_GUI: u8 = 0xe7;

    /// The usage id of a lowercase ASCII letter.
    pub(crate) const fn letter(c: u8) -> u8 {
        A + (c - b'a')
    }
}

/// `KeyboardModifier` bits.
pub(crate) mod modifier {
    pub(crate) const CONTROL: u64 = 1 << 0;
    pub(crate) const SHIFT: u64 = 1 << 1;
    pub(crate) const LEFT_ALT: u64 = 1 << 2;
    pub(crate) const RIGHT_ALT: u64 = 1 << 3;
    pub(crate) const GUI: u64 = 1 << 4;
}

/// The size of one `KeyboardState` in bytes.
pub(crate) const KEYBOARD_STATE_SIZE: usize = 0x30;

/// A set of pressed keys, one bit per usage id.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct KeySet(pub(crate) [u64; 4]);

impl KeySet {
    /// Marks a key as pressed.
    pub(crate) fn set(&mut self, usage: u8) {
        self.0[usize::from(usage) / 64] |= 1 << (usage % 64);
    }

    /// True when the key is pressed.
    pub(crate) fn get(&self, usage: u8) -> bool {
        self.0[usize::from(usage) / 64] >> (usage % 64) & 1 == 1
    }

    /// The `KeyboardModifier` bits of the pressed modifier keys.
    pub(crate) fn modifiers(&self) -> u64 {
        let mut m = 0;
        if self.get(usage::LEFT_CONTROL) || self.get(usage::RIGHT_CONTROL) {
            m |= modifier::CONTROL;
        }
        if self.get(usage::LEFT_SHIFT) || self.get(usage::RIGHT_SHIFT) {
            m |= modifier::SHIFT;
        }
        if self.get(usage::LEFT_ALT) {
            m |= modifier::LEFT_ALT;
        }
        if self.get(usage::RIGHT_ALT) {
            m |= modifier::RIGHT_ALT;
        }
        if self.get(usage::LEFT_GUI) || self.get(usage::RIGHT_GUI) {
            m |= modifier::GUI;
        }
        m
    }

    /// The 0x30 bytes of a `KeyboardState`: sampling number, modifiers, then the key bitmap.
    pub(crate) fn to_state_bytes(&self, sampling: u64) -> [u8; KEYBOARD_STATE_SIZE] {
        let mut b = [0u8; KEYBOARD_STATE_SIZE];
        b[0..8].copy_from_slice(&sampling.to_le_bytes());
        b[8..16].copy_from_slice(&self.modifiers().to_le_bytes());
        for (i, w) in self.0.iter().enumerate() {
            b[16 + 8 * i..24 + 8 * i].copy_from_slice(&w.to_le_bytes());
        }
        b
    }

    /// What the keyboard gives player 1, as nxnative maps it. J K U I are A B X Y, F and G are the stick clicks,
    /// Q and E are L and R, Z and C are ZL and ZR, Return is Plus and Tab is Minus. The arrows are the D-pad, or the
    /// right stick while Shift is held. W A S D are the left stick.
    pub(crate) fn to_pad(&self) -> RawKeys {
        const LETTERS: [(u8, u64); 10] = [
            (b'j', btn::A),
            (b'k', btn::B),
            (b'u', btn::X),
            (b'i', btn::Y),
            (b'f', btn::STICK_L),
            (b'g', btn::STICK_R),
            (b'q', btn::L),
            (b'e', btn::R),
            (b'z', btn::ZL),
            (b'c', btn::ZR),
        ];
        let mut buttons = 0;
        for (c, b) in LETTERS {
            if self.get(usage::letter(c)) {
                buttons |= b;
            }
        }
        if self.get(usage::RETURN) {
            buttons |= btn::PLUS;
        }
        if self.get(usage::TAB) {
            buttons |= btn::MINUS;
        }
        let axis = |neg: bool, pos: bool| match (neg, pos) {
            (true, false) => -32767,
            (false, true) => 32767,
            _ => 0,
        };
        let shift = self.modifiers() & modifier::SHIFT != 0;
        let (left, right, up, down) = (
            self.get(usage::LEFT),
            self.get(usage::RIGHT),
            self.get(usage::UP),
            self.get(usage::DOWN),
        );
        let mut sticks = [
            axis(self.get(usage::letter(b'a')), self.get(usage::letter(b'd'))),
            axis(self.get(usage::letter(b's')), self.get(usage::letter(b'w'))),
            0,
            0,
        ];
        if shift {
            sticks[2] = axis(left, right);
            sticks[3] = axis(down, up);
        } else {
            for (on, b) in [(left, btn::LEFT), (up, btn::UP), (right, btn::RIGHT), (down, btn::DOWN)] {
                if on {
                    buttons |= b;
                }
            }
        }
        RawKeys { buttons, sticks }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(us: &[u8]) -> KeySet {
        let mut k = KeySet::default();
        for &u in us {
            k.set(u);
        }
        k
    }

    #[test]
    fn letters_have_their_usage_ids() {
        assert_eq!(usage::letter(b'a'), 0x04);
        assert_eq!(usage::letter(b'j'), 0x0d);
        assert_eq!(usage::letter(b'z'), 0x1d);
    }

    #[test]
    fn the_gamepad_keys() {
        let k = keys(&[usage::letter(b'j'), usage::letter(b'e'), usage::RETURN, usage::TAB]).to_pad();
        assert_eq!(k.buttons, btn::A | btn::R | btn::PLUS | btn::MINUS);
        assert_eq!(k.sticks, [0; 4]);
    }

    #[test]
    fn wasd_is_the_left_stick() {
        let k = keys(&[usage::letter(b'w'), usage::letter(b'a')]).to_pad();
        assert_eq!(k.sticks, [-32767, 32767, 0, 0]);
        let k = keys(&[usage::letter(b'w'), usage::letter(b's')]).to_pad();
        assert_eq!(k.sticks[1], 0);
    }

    #[test]
    fn arrows_are_the_dpad_and_with_shift_the_right_stick() {
        let k = keys(&[usage::UP, usage::LEFT]).to_pad();
        assert_eq!(k.buttons, btn::UP | btn::LEFT);
        let k = keys(&[usage::UP, usage::LEFT, usage::LEFT_SHIFT]).to_pad();
        assert_eq!(k.buttons, 0);
        assert_eq!(k.sticks, [0, 0, -32767, 32767]);
    }

    #[test]
    fn the_state_has_the_keyboard_layout() {
        let k = keys(&[usage::letter(b'a'), usage::LEFT_SHIFT, usage::LEFT_GUI]);
        let b = k.to_state_bytes(7);
        assert_eq!(&b[0..8], &7u64.to_le_bytes());
        assert_eq!(&b[8..16], &(modifier::SHIFT | modifier::GUI).to_le_bytes());
        assert_eq!(&b[16..24], &(1u64 << 4).to_le_bytes());
        assert_eq!(&b[40..48], &(1u64 << (0xe1 - 0xc0) | 1u64 << (0xe3 - 0xc0)).to_le_bytes());
    }
}
