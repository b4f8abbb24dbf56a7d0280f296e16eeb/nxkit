//! Vibration functions. A value is stored at the call and played on the controller by the platform layer.

use nxkit_abi::export::Export;
use nxkit_abi::guest;

use super::{count, read_ref};
use crate::rumble::VibrationValue;
use crate::with;

/// The most handles of one `SendVibrationValues` call.
const MAX_BATCH: usize = 64;

/// The `VibrationDeviceInfo` of a handle: the device type (1 is a linear resonant actuator) and the position (1 is
/// left, 2 is right). Both are 0 when no controller is behind the handle.
pub(crate) fn vibration_info(handle: u32) -> (u32, u32) {
    match with(|i| i.vibration_target(handle)) {
        Some((_, motor)) => (1, 1 + motor as u32),
        None => (0, 0),
    }
}

fn read_value(addr: u64) -> Option<VibrationValue> {
    let mut b = [0u8; 16];
    guest::read(addr, &mut b).ok()?;
    Some(VibrationValue::from_bytes(&b))
}

extern "C" fn is_vibration_permitted() -> u32 {
    u32::from(with(|i| i.vibration_permitted && i.rumble_available()))
}

/// The handles are made from the npad id and the style, so a game that asks once at the start keeps working when
/// the controller comes later.
extern "C" fn get_device_handles(handles: u64, n: i32, id: u64, styles: u32) -> i32 {
    if handles == 0 || id == 0 {
        return 0;
    }
    let id = read_ref(id);
    let list = with(|i| i.vibration_handles(id, styles, count(n)));
    for (k, h) in list.iter().enumerate() {
        let _ = guest::write_u32(handles.wrapping_add(4 * k as u64), *h);
    }
    list.len() as i32
}

extern "C" fn initialize_device(handle: u64) {
    let handle = read_ref(handle);
    with(|i| i.send_vibration(handle, VibrationValue::IDLE));
}

extern "C" fn send_value(handle: u64, value: u64) {
    let handle = read_ref(handle);
    if let Some(value) = read_value(value) {
        with(|i| i.send_vibration(handle, value));
    }
}

/// The batch form the SDK's vibration graph flushes. All values reach the controller before the call returns.
extern "C" fn send_values(handles: u64, values: u64, n: i32) -> u32 {
    let n = count(n).min(MAX_BATCH);
    for k in 0..n as u64 {
        let handle = read_ref(handles.wrapping_add(4 * k));
        if let Some(value) = read_value(values.wrapping_add(16 * k)) {
            with(|i| i.send_vibration(handle, value));
        }
    }
    0
}

extern "C" fn get_device_info(info: u64, handle: u64) {
    let (kind, position) = vibration_info(read_ref(handle));
    let _ = guest::write_u32(info, kind);
    let _ = guest::write_u32(info.wrapping_add(4), position);
}

/// A device is mounted when a controller of the handle's style is on its npad.
extern "C" fn is_device_mounted(handle: u64) -> u32 {
    let handle = read_ref(handle);
    u32::from(with(|i| i.vibration_target(handle)).is_some())
}

extern "C" fn get_actual_value(out: u64, handle: u64) {
    let handle = read_ref(handle);
    let _ = guest::write(out, &with(|i| i.actual_vibration(handle)).to_bytes());
}

/// The vibration functions.
pub(super) fn list() -> Vec<Export> {
    vec![
        Export::new("_ZN2nn3hid20IsVibrationPermittedEv", is_vibration_permitted as usize),
        Export::new(
            "_ZN2nn3hid25GetVibrationDeviceHandlesEPNS0_21VibrationDeviceHandleEiRKjNS_4util10BitFlagSetILi32ENS0_12NpadStyleTagEEE",
            get_device_handles as usize,
        ),
        Export::new("_ZN2nn3hid25InitializeVibrationDeviceERKNS0_21VibrationDeviceHandleE", initialize_device as usize),
        Export::new(
            "_ZN2nn3hid18SendVibrationValueERKNS0_21VibrationDeviceHandleERKNS0_14VibrationValueE",
            send_value as usize,
        ),
        Export::new(
            "_ZN2nn3hid6detail19SendVibrationValuesEPKNS0_21VibrationDeviceHandleEPKNS0_14VibrationValueEi",
            send_values as usize,
        ),
        Export::new(
            "_ZN2nn3hid22GetVibrationDeviceInfoEPNS0_19VibrationDeviceInfoERKNS0_21VibrationDeviceHandleE",
            get_device_info as usize,
        ),
        Export::new("_ZN2nn3hid24IsVibrationDeviceMountedERKNS0_21VibrationDeviceHandleE", is_device_mounted as usize),
        Export::new(
            "_ZN2nn3hid29IsOunceVibrationDeviceMountedERKNS0_21VibrationDeviceHandleE",
            is_device_mounted as usize,
        ),
        Export::new(
            "_ZN2nn3hid23GetActualVibrationValueEPNS0_14VibrationValueERKNS0_21VibrationDeviceHandleE",
            get_actual_value as usize,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::guest_memory;

    #[test]
    fn a_handle_of_player_one_has_a_device() {
        // FullKey (3), npad No1 (0), the left motor (0).
        assert_eq!(vibration_info(3), (1, 1));
        assert_eq!(vibration_info(3 | 1 << 16), (1, 2));
    }

    #[test]
    fn a_wrong_handle_has_no_device() {
        assert_eq!(vibration_info(0), (0, 0));
        assert_eq!(vibration_info(3 | 2 << 16), (0, 0));
        assert_eq!(vibration_info(3 | 1 << 24), (0, 0));
        assert_eq!(vibration_info(5), (0, 0));
    }

    #[test]
    fn the_actual_value_is_the_last_one_sent() {
        let (base, _guard) = guest_memory();
        let sent = VibrationValue { amp_low: 0.5, freq_low: 160.0, amp_high: 0.25, freq_high: 320.0 };
        guest::write_u32(base, 3).ok();
        guest::write(base + 16, &sent.to_bytes()).ok();
        send_value(base, base + 16);
        get_actual_value(base + 64, base);
        let mut b = [0u8; 16];
        guest::read(base + 64, &mut b).ok();
        assert_eq!(VibrationValue::from_bytes(&b), sent.sanitized());
    }
}
