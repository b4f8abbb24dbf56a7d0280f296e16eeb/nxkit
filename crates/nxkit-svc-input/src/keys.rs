//! Keyboard keys as USB HID usage ids and the `KeyboardState` layout. The layout of player 1 is in `bind`.
//!
//! The usage ids are those of the USB HID usage table (page 7), which `nn::hid::KeyboardKey` also uses as bit
//! numbers. The platform layer reads `GCKeyboard` by key code and sets the bit of the matching usage here, so no
//! number of a `GCKeyCode` is assumed.

/// Usage ids that the map and the platform layer use.
pub(crate) mod usage {
    /// `a` is 0x04, so a letter is `A + (c - 'a')`.
    pub(crate) const A: u8 = 0x04;
    pub(crate) const ONE: u8 = 0x1e;
    pub(crate) const ZERO: u8 = 0x27;
    pub(crate) const RETURN: u8 = 0x28;
    pub(crate) const ESCAPE: u8 = 0x29;
    pub(crate) const BACKSPACE: u8 = 0x2a;
    pub(crate) const TAB: u8 = 0x2b;
    pub(crate) const SPACE: u8 = 0x2c;
    pub(crate) const HYPHEN: u8 = 0x2d;
    pub(crate) const EQUAL: u8 = 0x2e;
    pub(crate) const OPEN_BRACKET: u8 = 0x2f;
    pub(crate) const CLOSE_BRACKET: u8 = 0x30;
    pub(crate) const BACKSLASH: u8 = 0x31;
    pub(crate) const SEMICOLON: u8 = 0x33;
    pub(crate) const QUOTE: u8 = 0x34;
    pub(crate) const GRAVE: u8 = 0x35;
    pub(crate) const COMMA: u8 = 0x36;
    pub(crate) const PERIOD: u8 = 0x37;
    pub(crate) const SLASH: u8 = 0x38;
    pub(crate) const CAPS_LOCK: u8 = 0x39;
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
    pub(crate) fn to_state_bytes(self, sampling: u64) -> [u8; KEYBOARD_STATE_SIZE] {
        let mut b = [0u8; KEYBOARD_STATE_SIZE];
        b[0..8].copy_from_slice(&sampling.to_le_bytes());
        b[8..16].copy_from_slice(&self.modifiers().to_le_bytes());
        for (i, w) in self.0.iter().enumerate() {
            b[16 + 8 * i..24 + 8 * i].copy_from_slice(&w.to_le_bytes());
        }
        b
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
    fn the_state_has_the_keyboard_layout() {
        let k = keys(&[usage::letter(b'a'), usage::LEFT_SHIFT, usage::LEFT_GUI]);
        let b = k.to_state_bytes(7);
        assert_eq!(&b[0..8], &7u64.to_le_bytes());
        assert_eq!(&b[8..16], &(modifier::SHIFT | modifier::GUI).to_le_bytes());
        assert_eq!(&b[16..24], &(1u64 << 4).to_le_bytes());
        assert_eq!(
            &b[40..48],
            &(1u64 << (0xe1 - 0xc0) | 1u64 << (0xe3 - 0xc0)).to_le_bytes()
        );
    }
}
