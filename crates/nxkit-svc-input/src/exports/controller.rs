//! Controller details: the player LED pattern, the colors and the applets of the controller.

use nxkit_abi::export::Export;
use nxkit_abi::guest;

use super::read_ref;
use crate::with;

/// The player LEDs by npad, as the Switch lights them: bit 0 is LED 1.
const LED_PATTERNS: [u8; 8] = [0x1, 0x3, 0x7, 0xf, 0x9, 0x5, 0xd, 0x6];
/// The grey body and the black buttons of a controller, as `NpadControllerColor` { main, buttons } in RGBA.
const BODY_COLOR: u32 = 0xff82_8282;
const BUTTON_COLOR: u32 = 0xff0f_0f0f;

/// The LED pattern of an npad. Handheld and `Other` light LED 1.
pub(crate) fn player_led_pattern(id: u32) -> u8 {
    LED_PATTERNS.get(id as usize).copied().unwrap_or(1)
}

extern "C" fn get_player_led_pattern(id: u64) -> u32 {
    u32::from(player_led_pattern(read_ref(id)))
}

fn write_color(addr: u64) {
    let _ = guest::write_u32(addr, BODY_COLOR);
    let _ = guest::write_u32(addr.wrapping_add(4), BUTTON_COLOR);
}

extern "C" fn get_controller_color(color: u64, _id: u64) -> u32 {
    write_color(color);
    0
}

extern "C" fn get_controller_colors(left: u64, right: u64, _id: u64) -> u32 {
    write_color(left);
    write_color(right);
    0
}

/// The answer of the controller applet. `info` is a `ControllerSupportResultInfo` { int playerCount, u32 selectedId }
/// or null and `arg` a `ControllerSupportArg` that starts with the minimum and the maximum of players.
fn show_controller_support(info: u64, arg: u64) -> u32 {
    let min = guest::read_u8(arg).unwrap_or(0) as i8;
    let max = guest::read_u8(arg.wrapping_add(1)).unwrap_or(0) as i8;
    let (result, players, id) = with(|i| i.controller_support(min, max));
    if info != 0 {
        let _ = guest::write_u32(info, players as u32);
        let _ = guest::write_u32(info.wrapping_add(4), id);
    }
    result
}

extern "C" fn show_controller_support_with_info(info: u64, arg: u64) -> u32 {
    show_controller_support(info, arg)
}

extern "C" fn show_controller_support_arg(arg: u64) -> u32 {
    show_controller_support(0, arg)
}

stubs! { stub_list;
    "_ZN2nn3hid24ShowControllerStrapGuideEv" => strap_guide() -> u32 = 0;
    "_ZN2nn3hid24ShowControllerStrapGuideERKNS0_13StrapGuideArgE" => strap_guide_arg(u64) -> u32 = 0;
    "_ZN2nn3hid28ShowControllerFirmwareUpdateERKNS0_27ControllerFirmwareUpdateArgE" => firmware_update(u64) -> u32 = 0;
}

/// The controller functions.
pub(super) fn list() -> Vec<Export> {
    let mut v = vec![
        Export::new("_ZN2nn3hid19GetPlayerLedPatternERKj", get_player_led_pattern as usize),
        Export::new("_ZN2nn3hid22GetNpadControllerColorEPNS0_19NpadControllerColorERKj", get_controller_color as usize),
        Export::new(
            "_ZN2nn3hid22GetNpadControllerColorEPNS0_19NpadControllerColorES2_RKj",
            get_controller_colors as usize,
        ),
        Export::new(
            "_ZN2nn3hid21ShowControllerSupportEPNS0_27ControllerSupportResultInfoERKNS0_20ControllerSupportArgE",
            show_controller_support_with_info as usize,
        ),
        Export::new("_ZN2nn3hid21ShowControllerSupportERKNS0_20ControllerSupportArgE", show_controller_support_arg as usize),
    ];
    v.extend(stub_list());
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::guest_memory;

    #[test]
    fn the_led_patterns_follow_the_switch() {
        assert_eq!(player_led_pattern(0), 1);
        assert_eq!(player_led_pattern(3), 0xf);
        assert_eq!(player_led_pattern(7), 6);
        assert_eq!(player_led_pattern(0x20), 1);
    }

    #[test]
    fn both_colors_are_written() {
        let (base, _guard) = guest_memory();
        assert_eq!(get_controller_colors(base, base + 8, 0), 0);
        assert_eq!(guest::read_u32(base).unwrap_or(0), BODY_COLOR);
        assert_eq!(guest::read_u32(base + 12).unwrap_or(0), BUTTON_COLOR);
    }

    #[test]
    fn the_applet_is_closed_when_too_few_players_are_there() {
        let (base, _guard) = guest_memory();
        guest::write(base, &[100, 100]).ok();
        assert_eq!(show_controller_support(base + 16, base), 202 | 3101 << 9);
    }
}
