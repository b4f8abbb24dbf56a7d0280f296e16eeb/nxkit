//! Npad functions: the states, the supported ids and styles, the modes and the style update event.

use nxkit_abi::export::Export;
use nxkit_abi::guest;

use super::{count, put_all, read_ref};
use crate::npad::{PLAYERS, style};
use crate::{seam, with};

/// `GetNpadStates` of one style.
fn states(out: u64, n: i32, id: u64, style: u32) -> i32 {
    let n = count(n);
    if n == 0 {
        return 0;
    }
    let id = read_ref(id);
    let list = with(|i| i.npad_states(id, style, n));
    put_all(out, &list);
    n as i32
}

/// `GetNpadState` of one style.
fn state(out: u64, id: u64, style: u32) {
    let id = read_ref(id);
    let list = with(|i| i.npad_states(id, style, 1));
    put_all(out, &list);
}

macro_rules! npad_style {
    ($many:ident, $one:ident, $style:expr) => {
        extern "C" fn $many(out: u64, n: i32, id: u64) -> i32 {
            states(out, n, id, $style)
        }

        extern "C" fn $one(out: u64, id: u64) {
            state(out, id, $style)
        }
    };
}

npad_style!(states_full_key, state_full_key, style::FULL_KEY);
npad_style!(states_handheld, state_handheld, style::HANDHELD);
npad_style!(states_joy_dual, state_joy_dual, style::JOY_DUAL);
npad_style!(states_joy_left, state_joy_left, style::JOY_LEFT);
npad_style!(states_joy_right, state_joy_right, style::JOY_RIGHT);

extern "C" fn set_supported_ids(ids: u64, n: u64) {
    let n = n.min(64) as usize;
    let mut bytes = vec![0u8; n * 4];
    if guest::read(ids, &mut bytes).is_err() {
        bytes.clear();
    }
    let ids: Vec<u32> = bytes.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    with(|i| i.set_ids(&ids));
}

extern "C" fn set_supported_styles(styles: u32) {
    with(|i| i.style_set = styles);
}

extern "C" fn get_supported_styles() -> u32 {
    with(|i| i.style_set)
}

extern "C" fn get_style_set(id: u64) -> u32 {
    let id = read_ref(id);
    with(|i| i.layout().style_of(id))
}

extern "C" fn set_hold_type(kind: u32) {
    with(|i| i.hold_type = kind);
}

extern "C" fn get_hold_type() -> u32 {
    with(|i| i.hold_type)
}

extern "C" fn set_communication_mode(mode: u32) {
    with(|i| i.communication_mode = mode);
}

extern "C" fn get_communication_mode() -> u32 {
    with(|i| i.communication_mode)
}

extern "C" fn set_handheld_activation_mode(mode: u32) {
    with(|i| i.handheld_activation = mode);
}

extern "C" fn get_handheld_activation_mode() -> u32 {
    with(|i| i.handheld_activation)
}

/// Binds the event of the game, then signals it once if the npad has a controller, as the hid service does for a
/// controller that is attached at the start. The host binds the event outside the lock, because it runs SDK code.
extern "C" fn bind_style_set_update_event(id: u64, event: u64, clear_mode: u32) {
    let id = read_ref(id);
    seam::bind(event, clear_mode == 1);
    let connected = with(|i| {
        if (id as usize) < PLAYERS {
            i.style_events[id as usize] = Some(event);
        }
        i.layout().style_of(id) != 0
    });
    if connected {
        seam::signal(event);
    }
}

stubs! { stub_list;
    "_ZN2nn3hid14InitializeNpadEv" => initialize_npad() = ();
    "_ZN2nn3hid14DisconnectNpadERKj" => disconnect_npad(u64) = ();
    "_ZN2nn3hid20GetNpadJoyAssignmentERKj" => get_joy_assignment(u64) -> i32 = 0;
    "_ZN2nn3hid30SetNpadJoyAssignmentModeSingleERKj" => joy_single(u64) = ();
    "_ZN2nn3hid30SetNpadJoyAssignmentModeSingleERKjNS0_17NpadJoyDeviceTypeE" => joy_single_type(u64, u32) = ();
    "_ZN2nn3hid30SetNpadJoyAssignmentModeSingleEPjRKjNS0_17NpadJoyDeviceTypeE" => joy_single_out(u64, u64, u32) -> i32 = 0;
    "_ZN2nn3hid28SetNpadJoyAssignmentModeDualERKj" => joy_dual(u64) = ();
    "_ZN2nn3hid23MergeSingleJoyAsDualJoyERKjS2_" => merge_single_joy(u64, u64) -> u32 = 0;
    "_ZN2nn3hid18SwapNpadAssignmentERKjS2_" => swap_assignment(u64, u64) = ();
    "_ZN2nn3hid21StartLrAssignmentModeEv" => start_lr_assignment() = ();
    "_ZN2nn3hid20StopLrAssignmentModeEv" => stop_lr_assignment() = ();
}

/// The npad functions.
pub(super) fn list() -> Vec<Export> {
    let mut v = vec![
        Export::new("_ZN2nn3hid22SetSupportedNpadIdTypeEPKjm", set_supported_ids as usize),
        Export::new(
            "_ZN2nn3hid24SetSupportedNpadStyleSetENS_4util10BitFlagSetILi32ENS0_12NpadStyleTagEEE",
            set_supported_styles as usize,
        ),
        Export::new("_ZN2nn3hid24GetSupportedNpadStyleSetEv", get_supported_styles as usize),
        Export::new("_ZN2nn3hid15GetNpadStyleSetERKj", get_style_set as usize),
        Export::new("_ZN2nn3hid13GetNpadStatesEPNS0_16NpadFullKeyStateEiRKj", states_full_key as usize),
        Export::new("_ZN2nn3hid13GetNpadStatesEPNS0_17NpadHandheldStateEiRKj", states_handheld as usize),
        Export::new("_ZN2nn3hid13GetNpadStatesEPNS0_16NpadJoyDualStateEiRKj", states_joy_dual as usize),
        Export::new("_ZN2nn3hid13GetNpadStatesEPNS0_16NpadJoyLeftStateEiRKj", states_joy_left as usize),
        Export::new("_ZN2nn3hid13GetNpadStatesEPNS0_17NpadJoyRightStateEiRKj", states_joy_right as usize),
        Export::new("_ZN2nn3hid12GetNpadStateEPNS0_16NpadFullKeyStateERKj", state_full_key as usize),
        Export::new("_ZN2nn3hid12GetNpadStateEPNS0_17NpadHandheldStateERKj", state_handheld as usize),
        Export::new("_ZN2nn3hid12GetNpadStateEPNS0_16NpadJoyDualStateERKj", state_joy_dual as usize),
        Export::new("_ZN2nn3hid12GetNpadStateEPNS0_16NpadJoyLeftStateERKj", state_joy_left as usize),
        Export::new("_ZN2nn3hid12GetNpadStateEPNS0_17NpadJoyRightStateERKj", state_joy_right as usize),
        Export::new("_ZN2nn3hid18SetNpadJoyHoldTypeENS0_15NpadJoyHoldTypeE", set_hold_type as usize),
        Export::new("_ZN2nn3hid18GetNpadJoyHoldTypeEv", get_hold_type as usize),
        Export::new(
            "_ZN2nn3hid24SetNpadCommunicationModeENS0_21NpadCommunicationModeE",
            set_communication_mode as usize,
        ),
        Export::new("_ZN2nn3hid24GetNpadCommunicationModeEv", get_communication_mode as usize),
        Export::new(
            "_ZN2nn3hid29SetNpadHandheldActivationModeENS0_26NpadHandheldActivationModeE",
            set_handheld_activation_mode as usize,
        ),
        Export::new("_ZN2nn3hid29GetNpadHandheldActivationModeEv", get_handheld_activation_mode as usize),
        Export::new(
            "_ZN2nn3hid27BindNpadStyleSetUpdateEventERKjPNS_2os15SystemEventTypeENS3_14EventClearModeE",
            bind_style_set_update_event as usize,
        ),
    ];
    v.extend(stub_list());
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::guest_memory;

    #[test]
    fn states_write_n_entries_newest_first() {
        let (base, _guard) = guest_memory();
        guest::write_u32(base, 0).ok();
        let n = states(base + 0x100, 3, base, style::FULL_KEY);
        assert_eq!(n, 3);
        let first = guest::read_i64(base + 0x100).unwrap_or(0);
        let second = guest::read_i64(base + 0x100 + 0x28).unwrap_or(0);
        assert_eq!(first - 1, second);
    }

    #[test]
    fn a_count_of_zero_writes_nothing() {
        let (base, _guard) = guest_memory();
        assert_eq!(states(base, 0, base, style::FULL_KEY), 0);
        assert_eq!(states(base, -4, base, style::FULL_KEY), 0);
    }

    #[test]
    fn the_hold_type_is_kept() {
        set_hold_type(1);
        assert_eq!(get_hold_type(), 1);
        set_hold_type(0);
    }
}
