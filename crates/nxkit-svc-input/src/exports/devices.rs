//! Mouse, keyboard and debug pad functions.
//!
//! The keyboard and the mouse buttons come from `GCKeyboard` and `GCMouse` when the Mac has them. The debug pad is
//! never connected.

use nxkit_abi::export::Export;
use nxkit_abi::guest;

use super::{count, history, put_all};
use crate::state::{MOUSE_STATE_SIZE, Input};
use crate::keys::KEYBOARD_STATE_SIZE;
use crate::with;

/// The size of one `DebugPadState` in bytes: sampling number, attributes, buttons and two sticks.
const DEBUG_PAD_STATE_SIZE: usize = 32;

fn debug_pad_state(input: &mut Input) -> [u8; DEBUG_PAD_STATE_SIZE] {
    let mut b = [0u8; DEBUG_PAD_STATE_SIZE];
    b[..8].copy_from_slice(&input.next_sampling().to_le_bytes());
    b
}

extern "C" fn get_mouse_state(out: u64) {
    let state = with(Input::mouse_state);
    let _ = guest::write(out, &state);
}

/// Every requested entry, newest first, with consecutive sampling numbers as `GetNpadStates` fills them.
extern "C" fn get_mouse_states(out: u64, n: i32) -> i32 {
    let n = count(n);
    if n == 0 {
        return 0;
    }
    let list = history::<MOUSE_STATE_SIZE>(with(Input::mouse_state), n);
    put_all(out, &list);
    n as i32
}

extern "C" fn get_keyboard_state(out: u64) {
    let state = with(Input::keyboard_state);
    let _ = guest::write(out, &state);
}

extern "C" fn get_keyboard_states(out: u64, n: i32) -> i32 {
    let n = count(n);
    if n == 0 {
        return 0;
    }
    let list = history::<KEYBOARD_STATE_SIZE>(with(Input::keyboard_state), n);
    put_all(out, &list);
    n as i32
}

extern "C" fn get_debug_pad_state(out: u64) {
    let state = with(debug_pad_state);
    let _ = guest::write(out, &state);
}

extern "C" fn get_debug_pad_states(out: u64, n: i32) -> i32 {
    let n = count(n);
    if n == 0 {
        return 0;
    }
    let list = history::<DEBUG_PAD_STATE_SIZE>(with(debug_pad_state), n);
    put_all(out, &list);
    n as i32
}

stubs! { stub_list;
    "_ZN2nn3hid15InitializeMouseEv" => initialize_mouse() = ();
    "_ZN2nn3hid18InitializeKeyboardEv" => initialize_keyboard() = ();
    "_ZN2nn3hid18InitializeDebugPadEv" => initialize_debug_pad() = ();
    "_ZN2nn3kpr13GetKeyCodeMapEPNS0_10KeyCodeMapE" => get_key_code_map(u64) = ();
}

/// The mouse, keyboard and debug pad functions.
pub(super) fn list() -> Vec<Export> {
    let mut v = vec![
        Export::new("_ZN2nn3hid13GetMouseStateEPNS0_10MouseStateE", get_mouse_state as usize),
        Export::new("_ZN2nn3hid14GetMouseStatesEPNS0_10MouseStateEi", get_mouse_states as usize),
        Export::new("_ZN2nn3hid16GetKeyboardStateEPNS0_13KeyboardStateE", get_keyboard_state as usize),
        Export::new("_ZN2nn3hid17GetKeyboardStatesEPNS0_13KeyboardStateEi", get_keyboard_states as usize),
        Export::new("_ZN2nn3hid16GetDebugPadStateEPNS0_13DebugPadStateE", get_debug_pad_state as usize),
        Export::new("_ZN2nn3hid17GetDebugPadStatesEPNS0_13DebugPadStateEi", get_debug_pad_states as usize),
    ];
    v.extend(stub_list());
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::guest_memory;

    #[test]
    fn the_debug_pad_is_never_connected() {
        let (base, _guard) = guest_memory();
        guest::fill(base, 0x200, 0xff).ok();
        assert_eq!(get_debug_pad_states(base, 3), 3);
        let first = guest::read_i64(base).unwrap_or(0);
        let second = guest::read_i64(base + 32).unwrap_or(0);
        assert_eq!(first - 1, second);
        assert_eq!(guest::read_u64(base + 8).unwrap_or(1), 0);
        assert_eq!(guest::read_u64(base + 24).unwrap_or(1), 0);
    }

    #[test]
    fn keyboard_histories_have_the_state_size() {
        let (base, _guard) = guest_memory();
        guest::fill(base, 0x400, 0xff).ok();
        assert_eq!(get_keyboard_states(base, 2), 2);
        assert_eq!(guest::read_u8(base + 2 * KEYBOARD_STATE_SIZE as u64).unwrap_or(0), 0xff);
    }

    #[test]
    fn the_mouse_has_no_buttons_without_a_mouse() {
        let (base, _guard) = guest_memory();
        guest::fill(base, 0x100, 0xff).ok();
        get_mouse_state(base);
        assert_eq!(guest::read_u32(base + 32).unwrap_or(1), 0);
        assert_eq!(guest::read_u32(base + 36).unwrap_or(1), 0);
    }
}
