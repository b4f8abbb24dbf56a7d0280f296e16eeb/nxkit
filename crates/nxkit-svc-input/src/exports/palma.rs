//! Poké Ball Plus (Palma) functions. The host has none, so every call on a connection handle fails as for a handle
//! that is not connected.

use nxkit_abi::export::Export;
use nxkit_abi::guest;

/// `ResultPalmaInvalidHandle`: module 202, description 3302.
const INVALID_HANDLE: u32 = 202 | 3302 << 9;

extern "C" fn get_connection_handle(handle: u64, _id: u64) -> u32 {
    if handle != 0 {
        let _ = guest::write_u64(handle, 0);
    }
    INVALID_HANDLE
}

stubs! { stub_list;
    "_ZN2nn3hid17SetPalmaBoostModeEb" => boost_mode(u32) = ();
    "_ZN2nn3hid24EnableAnyPalmaConnectionEv" => enable_any() -> u32 = 0;
    "_ZN2nn3hid25DisableAnyPalmaConnectionEv" => disable_any() -> u32 = 0;
    "_ZN2nn3hid15InitializePalmaERKNS0_21PalmaConnectionHandleE" => initialize(u64) -> u32 = INVALID_HANDLE;
    "_ZN2nn3hid31BindPalmaOperationCompleteEventERKNS0_21PalmaConnectionHandleEPNS_2os15SystemEventTypeENS4_14EventClearModeE" => bind_event(u64, u64, u32) -> u32 = INVALID_HANDLE;
    "_ZN2nn3hid21GetPalmaOperationInfoEPNS0_18PalmaOperationInfoERKNS0_21PalmaConnectionHandleE" => operation_info(u64, u64) -> u32 = INVALID_HANDLE;
    "_ZN2nn3hid17PlayPalmaActivityERKNS0_21PalmaConnectionHandleERKt" => play_activity(u64, u64) -> u32 = INVALID_HANDLE;
    "_ZN2nn3hid18SetPalmaFrModeTypeERKNS0_21PalmaConnectionHandleERKNS0_15PalmaFrModeTypeE" => set_fr_mode(u64, u64) -> u32 = INVALID_HANDLE;
    "_ZN2nn3hid13ReadPalmaStepERKNS0_21PalmaConnectionHandleE" => read_step(u64) -> u32 = INVALID_HANDLE;
    "_ZN2nn3hid15EnablePalmaStepERKNS0_21PalmaConnectionHandleEb" => enable_step(u64, u32) -> u32 = INVALID_HANDLE;
    "_ZN2nn3hid14ResetPalmaStepERKNS0_21PalmaConnectionHandleE" => reset_step(u64) -> u32 = INVALID_HANDLE;
    "_ZN2nn3hid27ReadPalmaApplicationSectionERKNS0_21PalmaConnectionHandleEim" => read_application_section(u64, u32, u64) -> u32 = INVALID_HANDLE;
    "_ZN2nn3hid28WritePalmaApplicationSectionERKNS0_21PalmaConnectionHandleEimRKNS0_35PalmaApplicationSectionAccessBufferE" => write_application_section(u64, u32, u64, u64) -> u32 = INVALID_HANDLE;
    "_ZN2nn3hid19ReadPalmaUniqueCodeERKNS0_21PalmaConnectionHandleE" => read_unique_code(u64) -> u32 = INVALID_HANDLE;
    "_ZN2nn3hid25SetPalmaUniqueCodeInvalidERKNS0_21PalmaConnectionHandleE" => unique_code_invalid(u64) -> u32 = INVALID_HANDLE;
    "_ZN2nn3hid23WritePalmaActivityEntryERKNS0_21PalmaConnectionHandleERKtPKNS0_18PalmaActivityEntryE" => write_activity_entry(u64, u64, u64) -> u32 = INVALID_HANDLE;
    "_ZN2nn3hid28WritePalmaRgbLedPatternEntryERKNS0_21PalmaConnectionHandleERKtPKvm" => write_led_pattern(u64, u64, u64, u64) -> u32 = INVALID_HANDLE;
    "_ZN2nn3hid19WritePalmaWaveEntryERKNS0_21PalmaConnectionHandleERKNS0_12PalmaWaveSetERKtPKvmm" => write_wave_entry(u64, u64, u64, u64, u64, u64) -> u32 = INVALID_HANDLE;
    "_ZN2nn3hid37GetPalmaDataBaseIdentificationVersionERKNS0_21PalmaConnectionHandleE" => get_database_version(u64) -> u32 = INVALID_HANDLE;
    "_ZN2nn3hid37SetPalmaDataBaseIdentificationVersionERKNS0_21PalmaConnectionHandleEi" => set_database_version(u64, u32) -> u32 = INVALID_HANDLE;
    "_ZN2nn3hid19SuspendPalmaFeatureERKNS0_21PalmaConnectionHandleERKNS_4util10BitFlagSetILi32ENS0_12PalmaFeatureEEE" => suspend_feature(u64, u64) -> u32 = INVALID_HANDLE;
    "_ZN2nn3hid16ReadPalmaPlayLogERKNS0_21PalmaConnectionHandleERKt" => read_play_log(u64, u64) -> u32 = INVALID_HANDLE;
    "_ZN2nn3hid17ResetPalmaPlayLogERKNS0_21PalmaConnectionHandleERKt" => reset_play_log(u64, u64) -> u32 = INVALID_HANDLE;
}

/// The Palma functions.
pub(super) fn list() -> Vec<Export> {
    let mut v = vec![Export::new(
        "_ZN2nn3hid24GetPalmaConnectionHandleEPNS0_21PalmaConnectionHandleERKj",
        get_connection_handle as *const () as usize,
    )];
    v.extend(stub_list());
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::guest_memory;

    #[test]
    fn no_connection_handle_is_given() {
        let (base, _guard) = guest_memory();
        guest::fill(base, 8, 0xff).ok();
        assert_eq!(get_connection_handle(base, base + 8), INVALID_HANDLE);
        assert_eq!(guest::read_u64(base).unwrap_or(1), 0);
        assert_eq!(get_connection_handle(0, 0), INVALID_HANDLE);
    }

    #[test]
    fn calls_on_a_handle_fail() {
        assert_eq!(read_step(0), INVALID_HANDLE);
        assert_eq!(enable_any(), 0);
    }
}
