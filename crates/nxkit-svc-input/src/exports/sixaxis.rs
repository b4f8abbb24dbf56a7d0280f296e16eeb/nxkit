//! Six-axis sensor functions. The state comes from `GCMotion` at the moment of the call.

use nxkit_abi::export::Export;
use nxkit_abi::guest;

use super::{count, put_all, read_ref};
use crate::npad::style;
use crate::state::SIX_AXIS_STATE_SIZE;
use crate::with;

/// The states of a handle, newest first. A handle without a controller reads as zero entries and a count of 0.
fn handle_states(out: u64, n: usize, handle: u32) -> i32 {
    if n == 0 {
        return 0;
    }
    let list = with(|i| i.six_axis_handle_states(handle, n));
    if list.is_empty() {
        let _ = guest::fill(out, (n * SIX_AXIS_STATE_SIZE) as u64, 0);
        return 0;
    }
    put_all(out, &list);
    list.len() as i32
}

/// `GetSixAxisSensorHandles`: two sensors for the handheld and dual styles, one for the others.
extern "C" fn get_handles(handles: u64, n: i32, id: u64, styles: u32) -> i32 {
    let wanted = if styles & (style::HANDHELD | style::JOY_DUAL) != 0 {
        2
    } else {
        1
    };
    let k = wanted.min(n.max(0));
    let id = read_ref(id) & 0xff;
    let bit = if styles == 0 {
        0
    } else {
        styles.trailing_zeros()
    };
    for i in 0..k as u32 {
        let _ = guest::write_u32(
            handles.wrapping_add(4 * u64::from(i)),
            bit | id << 8 | i << 16,
        );
    }
    k
}

extern "C" fn get_states(out: u64, n: i32, handle: u64) -> i32 {
    handle_states(out, count(n), read_ref(handle))
}

extern "C" fn get_state(out: u64, handle: u64) {
    handle_states(out, 1, read_ref(handle));
}

extern "C" fn is_at_rest(handle: u64) -> u32 {
    let handle = read_ref(handle);
    u32::from(with(|i| i.handle_at_rest(handle)))
}

extern "C" fn get_fusion_parameters(a: u64, b: u64, _handle: u64) {
    let _ = guest::write_f32(a, 0.0);
    let _ = guest::write_f32(b, 0.0);
}

stubs! { stub_list;
    "_ZN2nn3hid18StartSixAxisSensorERKNS0_19SixAxisSensorHandleE" => start(u64) = ();
    "_ZN2nn3hid17StopSixAxisSensorERKNS0_19SixAxisSensorHandleE" => stop(u64) = ();
    "_ZN2nn3hid20FinalizeSensorFusionEv" => finalize_fusion() = ();
    "_ZN2nn3hid25EnableSixAxisSensorFusionERKNS0_19SixAxisSensorHandleEb" => enable_fusion(u64, u32) = ();
    "_ZN2nn3hid28IsSixAxisSensorFusionEnabledERKNS0_19SixAxisSensorHandleE" => is_fusion_enabled(u64) -> u32 = 0;
    "_ZN2nn3hid25GetGyroscopeZeroDriftModeERKNS0_19SixAxisSensorHandleE" => get_zero_drift(u64) -> i32 = 1;
    "_ZN2nn3hid25SetGyroscopeZeroDriftModeERKNS0_19SixAxisSensorHandleERKNS0_22GyroscopeZeroDriftModeE" => set_zero_drift(u64, u64) = ();
    "_ZN2nn3hid32SetSixAxisSensorFusionParametersERKNS0_19SixAxisSensorHandleEff" => set_fusion_parameters(u64) = ();
    "_ZN2nn3hid34ResetSixAxisSensorFusionParametersERKNS0_19SixAxisSensorHandleE" => reset_fusion_parameters(u64) = ();
    "_ZN2nn3hid41IsFirmwareUpdateAvailableForSixAxisSensorERKNS0_19SixAxisSensorHandleE" => six_axis_firmware(u64) -> u32 = 0;
    "_ZN2nn3hid28InitializeSevenSixAxisSensorEPvm" => seven_initialize(u64, u64) -> u32 = 0;
    "_ZN2nn3hid26FinalizeSevenSixAxisSensorEv" => seven_finalize() -> u32 = 0;
    "_ZN2nn3hid23StartSevenSixAxisSensorEv" => seven_start() -> u32 = 0;
    "_ZN2nn3hid22StopSevenSixAxisSensorEv" => seven_stop() -> u32 = 0;
    "_ZN2nn3hid32ResetSevenSixAxisSensorTimestampEv" => seven_reset_timestamp() = ();
    "_ZN2nn3hid27GetSevenSixAxisSensorStatesEPNS0_23SevenSixAxisSensorStateEi" => seven_states(u64, i32) -> i32 = 0;
    "_ZN2nn3hid26IsSevenSixAxisSensorAtRestEv" => seven_at_rest() -> u32 = 1;
}

/// The six-axis functions.
pub(super) fn list() -> Vec<Export> {
    let mut v = vec![
        Export::new(
            "_ZN2nn3hid23GetSixAxisSensorHandlesEPNS0_19SixAxisSensorHandleEiRKjNS_4util10BitFlagSetILi32ENS0_12NpadStyleTagEEE",
            get_handles as usize,
        ),
        Export::new(
            "_ZN2nn3hid22GetSixAxisSensorStatesEPNS0_18SixAxisSensorStateEiRKNS0_19SixAxisSensorHandleE",
            get_states as usize,
        ),
        Export::new(
            "_ZN2nn3hid21GetSixAxisSensorStateEPNS0_18SixAxisSensorStateERKNS0_19SixAxisSensorHandleE",
            get_state as usize,
        ),
        Export::new(
            "_ZN2nn3hid21IsSixAxisSensorAtRestERKNS0_19SixAxisSensorHandleE",
            is_at_rest as usize,
        ),
        Export::new(
            "_ZN2nn3hid32GetSixAxisSensorFusionParametersEPfS1_RKNS0_19SixAxisSensorHandleE",
            get_fusion_parameters as usize,
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
    fn handheld_has_two_sensors_and_full_key_one() {
        let (base, _guard) = guest_memory();
        guest::write_u32(base, 0).ok();
        assert_eq!(get_handles(base + 16, 4, base, style::HANDHELD), 2);
        assert_eq!(guest::read_u32(base + 16).unwrap_or(0), 1);
        assert_eq!(guest::read_u32(base + 20).unwrap_or(0), 1 | 1 << 16);
        assert_eq!(get_handles(base + 16, 4, base, style::FULL_KEY), 1);
        assert_eq!(get_handles(base + 16, 1, base, style::HANDHELD), 1);
        assert_eq!(get_handles(base + 16, 0, base, style::HANDHELD), 0);
    }

    #[test]
    fn a_handle_without_a_controller_reads_as_zeros() {
        let (base, _guard) = guest_memory();
        guest::fill(base, 0x200, 0xaa).ok();
        guest::write_u32(base, 0x1f | 0x7 << 8).ok();
        assert_eq!(get_states(base + 0x100, 2, base), 0);
        assert_eq!(guest::read_u32(base + 0x100).unwrap_or(1), 0);
        assert_eq!(guest::read_u32(base + 0x100 + 0x5c).unwrap_or(1), 0);
    }

    #[test]
    fn the_fusion_parameters_are_zero() {
        let (base, _guard) = guest_memory();
        guest::fill(base, 16, 0xff).ok();
        get_fusion_parameters(base, base + 4, base + 8);
        assert_eq!(guest::read_u32(base).unwrap_or(1), 0);
        assert_eq!(guest::read_u32(base + 4).unwrap_or(1), 0);
    }
}
