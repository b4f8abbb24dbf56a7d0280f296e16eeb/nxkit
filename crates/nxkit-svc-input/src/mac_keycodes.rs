//! macOS virtual key codes (`NSEvent.keyCode`, the `kVK_*` constants of Carbon's `Events.h`) as the USB HID usage ids
//! of `keys`. `NXKIT_KEYMAP` names keys by their virtual key code, the platform layer reads them by usage id.
//!
//! The table has the keys that the platform layer reads: letters, digits, the punctuation of the main block, the
//! editing and arrow keys, F1 to F12 and the modifiers. The keypad, F13 to F20, the media keys and the keys of the
//! ISO and JIS layouts have a code but no usage here, so the platform layer does not read them.

use crate::keys::usage;

/// `(virtual key code, usage id)` of every key that the platform layer reads and that has a virtual key code.
const TABLE: &[(u8, u8)] = &[
    (0, usage::letter(b'a')),
    (1, usage::letter(b's')),
    (2, usage::letter(b'd')),
    (3, usage::letter(b'f')),
    (4, usage::letter(b'h')),
    (5, usage::letter(b'g')),
    (6, usage::letter(b'z')),
    (7, usage::letter(b'x')),
    (8, usage::letter(b'c')),
    (9, usage::letter(b'v')),
    (11, usage::letter(b'b')),
    (12, usage::letter(b'q')),
    (13, usage::letter(b'w')),
    (14, usage::letter(b'e')),
    (15, usage::letter(b'r')),
    (16, usage::letter(b'y')),
    (17, usage::letter(b't')),
    (18, usage::ONE),
    (19, usage::ONE + 1),
    (20, usage::ONE + 2),
    (21, usage::ONE + 3),
    (22, usage::ONE + 5),
    (23, usage::ONE + 4),
    (24, usage::EQUAL),
    (25, usage::ONE + 8),
    (26, usage::ONE + 6),
    (27, usage::HYPHEN),
    (28, usage::ONE + 7),
    (29, usage::ZERO),
    (30, usage::CLOSE_BRACKET),
    (31, usage::letter(b'o')),
    (32, usage::letter(b'u')),
    (33, usage::OPEN_BRACKET),
    (34, usage::letter(b'i')),
    (35, usage::letter(b'p')),
    (36, usage::RETURN),
    (37, usage::letter(b'l')),
    (38, usage::letter(b'j')),
    (39, usage::QUOTE),
    (40, usage::letter(b'k')),
    (41, usage::SEMICOLON),
    (42, usage::BACKSLASH),
    (43, usage::COMMA),
    (44, usage::SLASH),
    (45, usage::letter(b'n')),
    (46, usage::letter(b'm')),
    (47, usage::PERIOD),
    (48, usage::TAB),
    (49, usage::SPACE),
    (50, usage::GRAVE),
    (51, usage::BACKSPACE),
    (53, usage::ESCAPE),
    (54, usage::RIGHT_GUI),
    (55, usage::LEFT_GUI),
    (56, usage::LEFT_SHIFT),
    (57, usage::CAPS_LOCK),
    (58, usage::LEFT_ALT),
    (59, usage::LEFT_CONTROL),
    (60, usage::RIGHT_SHIFT),
    (61, usage::RIGHT_ALT),
    (62, usage::RIGHT_CONTROL),
    (96, usage::F1 + 4),
    (97, usage::F1 + 5),
    (98, usage::F1 + 6),
    (99, usage::F1 + 2),
    (100, usage::F1 + 7),
    (101, usage::F1 + 8),
    (103, usage::F1 + 10),
    (109, usage::F1 + 9),
    (111, usage::F1 + 11),
    (114, usage::INSERT),
    (115, usage::HOME),
    (116, usage::PAGE_UP),
    (117, usage::DELETE_FORWARD),
    (118, usage::F1 + 3),
    (119, usage::END),
    (120, usage::F1 + 1),
    (121, usage::PAGE_DOWN),
    (122, usage::F1),
    (123, usage::LEFT),
    (124, usage::RIGHT),
    (125, usage::DOWN),
    (126, usage::UP),
];

/// The usage id of a virtual key code. `None` for a code of a key that the platform layer does not read.
pub(crate) fn usage_of(code: u8) -> Option<u8> {
    TABLE.iter().find(|(c, _)| *c == code).map(|(_, u)| *u)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keys_of_the_old_engine_layout_have_their_usages() {
        let cases: &[(u8, u8)] = &[
            (38, usage::letter(b'j')),
            (40, usage::letter(b'k')),
            (32, usage::letter(b'u')),
            (34, usage::letter(b'i')),
            (13, usage::letter(b'w')),
            (0, usage::letter(b'a')),
            (1, usage::letter(b's')),
            (2, usage::letter(b'd')),
            (36, usage::RETURN),
            (48, usage::TAB),
            (49, usage::SPACE),
            (126, usage::UP),
            (123, usage::LEFT),
            (15, usage::letter(b'r')),
        ];
        for &(code, u) in cases {
            assert_eq!(usage_of(code), Some(u), "key code {code}");
        }
    }

    #[test]
    fn digits_and_function_keys_follow_the_mac_order() {
        assert_eq!(usage_of(18), Some(usage::ONE));
        assert_eq!(usage_of(23), Some(usage::ONE + 4));
        assert_eq!(usage_of(22), Some(usage::ONE + 5));
        assert_eq!(usage_of(29), Some(usage::ZERO));
        assert_eq!(usage_of(122), Some(usage::F1));
        assert_eq!(usage_of(111), Some(usage::F1 + 11));
    }

    #[test]
    fn keys_that_are_not_read_have_no_usage() {
        // ISO section, kVK_Function, keypad decimal, keypad 0, F13, a free code, past the table
        for code in [10, 63, 65, 82, 105, 52, 127, 200] {
            assert_eq!(usage_of(code), None, "key code {code}");
        }
    }

    #[test]
    fn no_two_codes_share_a_usage() {
        for (i, (a, ua)) in TABLE.iter().enumerate() {
            for (b, ub) in &TABLE[i + 1..] {
                assert!(a != b && ua != ub, "{a} and {b}");
            }
        }
    }
}
