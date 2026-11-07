//! `nn::nfc` and `nn::nfp` take-overs. Source: nxnative `dm/host/hle_sdk16.c`, sections "nfc" and "nfp".
//!
//! The host has no NFC reader. `nn::nfp` reports one device on the connected controller that never finds a tag, as a
//! console does with no amiibo at hand. Every operation that needs a tag returns the result "tag not found". The
//! `nn::nfc` library answers as a console whose NFC is switched off.

use super::{log, log_once, put_u32, put_u8, put_u64, put_zero, zero};
use crate::event;
use crate::seams::seams;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Module 115 (nfp), description 97.
const NO_TAG: u32 = 115 | 97 << 9;
/// `'NFP0'`, the id of the one device.
const DEVICE_ID: u32 = 0x4e46_5030;
const STATE_INIT: u32 = 0;
const STATE_SEARCH: u32 = 1;

static NFC_STATE: AtomicU32 = AtomicU32::new(0);
static NFP_LIBRARY: AtomicU32 = AtomicU32::new(0);
static NFP_DEVICE: AtomicU32 = AtomicU32::new(STATE_INIT);
static NO_TAG_LOGGED: AtomicBool = AtomicBool::new(false);

fn no_tag_result() -> u32 {
    log_once(
        &NO_TAG_LOGGED,
        "nfp: operations that need an amiibo tag return no tag found (the host has no NFC)",
    );
    NO_TAG
}

/// Every nfp operation that needs a tag.
extern "C" fn no_tag() -> u32 {
    no_tag_result()
}

extern "C" fn nfc_initialize() {
    NFC_STATE.store(1, Ordering::SeqCst);
    log("nfc::Initialize: NFC is disabled (the host has no NFC reader)");
}

extern "C" fn nfc_finalize() {
    NFC_STATE.store(0, Ordering::SeqCst);
}

extern "C" fn nfc_state() -> u32 {
    NFC_STATE.load(Ordering::SeqCst)
}

extern "C" fn nfp_initialize() {
    NFP_LIBRARY.store(1, Ordering::SeqCst);
    log("nfp::Initialize: the host has one NFC device and finds no amiibo tag");
}

extern "C" fn nfp_finalize() {
    NFP_LIBRARY.store(0, Ordering::SeqCst);
}

extern "C" fn nfp_library_state() -> u32 {
    NFP_LIBRARY.load(Ordering::SeqCst)
}

/// `AttachAvailabilityChangeEvent`, `AttachActivateEvent` and `AttachDeactivateEvent`: bound, never signalled. The
/// event is the first argument in every form.
extern "C" fn attach_event(ev: u64) -> u32 {
    event::bind(ev, false);
    0
}

/// `ListDevices(DeviceHandle*, int*, int)`: one device.
extern "C" fn list_devices(out: u64, count: u64, max: u32) -> u32 {
    put_u32(count, 0);
    if max as i32 >= 1 {
        put_zero(out, 8);
        put_u32(out, DEVICE_ID);
        put_u32(count, 1);
    }
    0
}

/// `GetNpadId(uint32_t*, const DeviceHandle&)`: the controller that is connected now.
extern "C" fn npad_id(out: u64, _handle: u64) -> u32 {
    put_u32(out, seams().hid_connected_npad.map_or(0, |f| f()));
    0
}

extern "C" fn start_detection(_handle: u64) -> u32 {
    NFP_DEVICE.store(STATE_SEARCH, Ordering::SeqCst);
    0
}

extern "C" fn stop_detection(_handle: u64) -> u32 {
    NFP_DEVICE.store(STATE_INIT, Ordering::SeqCst);
    0
}

extern "C" fn device_state(_handle: u64) -> u32 {
    NFP_DEVICE.load(Ordering::SeqCst)
}

/// `GetApplicationArea(void*, uint64_t*, const DeviceHandle&, uint64_t)`: no data.
extern "C" fn get_application_area(_buf: u64, size: u64) -> u32 {
    put_u64(size, 0);
    no_tag_result()
}

/// `StartNicknameAndOwnerSettings(DeviceHandle*, bool*, ...)`: nothing changed.
extern "C" fn nickname_settings(_handle: u64, changed: u64) -> u32 {
    put_u8(changed, 0);
    no_tag_result()
}

table! {
    "_ZN2nn3nfc10InitializeEv" => nfc_initialize,
    "_ZN2nn3nfc8FinalizeEv" => nfc_finalize,
    "_ZN2nn3nfc8GetStateEv" => nfc_state,
    "_ZN2nn3nfc12IsNfcEnabledEv" => zero,
    "_ZN2nn3nfp10InitializeEv" => nfp_initialize,
    "_ZN2nn3nfp37InitializeWithExtendedApplicationAreaEv" => nfp_initialize,
    "_ZN2nn3nfp29AttachAvailabilityChangeEventEPNS_2os15SystemEventTypeE" => attach_event,
    "_ZN2nn3nfp11ListDevicesEPNS0_12DeviceHandleEPii" => list_devices,
    "_ZN2nn3nfp9GetNpadIdEPjRKNS0_12DeviceHandleE" => npad_id,
    "_ZN2nn3nfp14StartDetectionERKNS0_12DeviceHandleE" => start_detection,
    "_ZN2nn3nfp13StopDetectionERKNS0_12DeviceHandleE" => stop_detection,
    "_ZN2nn3nfp14GetDeviceStateERKNS0_12DeviceHandleE" => device_state,
    "_ZN2nn3nfp10GetTagInfoEPNS0_7TagInfoERKNS0_12DeviceHandleE" => no_tag,
    "_ZN2nn3nfp12GetModelInfoEPNS0_9ModelInfoERKNS0_12DeviceHandleE" => no_tag,
    "_ZN2nn3nfp5MountERKNS0_12DeviceHandleENS0_9ModelTypeENS0_11MountTargetE" => no_tag,
    "_ZN2nn3nfp19AttachActivateEventEPNS_2os15SystemEventTypeERKNS0_12DeviceHandleE" => attach_event,
    "_ZN2nn3nfp21AttachDeactivateEventEPNS_2os15SystemEventTypeERKNS0_12DeviceHandleE" => attach_event,
    "_ZN2nn3nfp5MountERKNS0_12DeviceHandleENS0_9ModelTypeE" => no_tag,
    "_ZN2nn3nfp7UnmountERKNS0_12DeviceHandleE" => no_tag,
    "_ZN2nn3nfp13GetCommonInfoEPNS0_10CommonInfoERKNS0_12DeviceHandleE" => no_tag,
    "_ZN2nn3nfp15GetRegisterInfoEPNS0_12RegisterInfoERKNS0_12DeviceHandleE" => no_tag,
    "_ZN2nn3nfp8GetStateEv" => nfp_library_state,
    "_ZN2nn3nfp8FinalizeEv" => nfp_finalize,
    "_ZN2nn3nfp5FlushERKNS0_12DeviceHandleE" => no_tag,
    "_ZN2nn3nfp19OpenApplicationAreaERKNS0_12DeviceHandleEj" => no_tag,
    "_ZN2nn3nfp18GetApplicationAreaEPvPmRKNS0_12DeviceHandleEm" => get_application_area,
    "_ZN2nn3nfp18SetApplicationAreaERKNS0_12DeviceHandleEPKvm" => no_tag,
    "_ZN2nn3nfp21CreateApplicationAreaERKNS0_12DeviceHandleERKNS0_25ApplicationAreaCreateInfoE" => no_tag,
    "_ZN2nn3nfp23RecreateApplicationAreaERKNS0_12DeviceHandleERKNS0_25ApplicationAreaCreateInfoE" => no_tag,
    "_ZN2nn3nfp26GetExtendedApplicationAreaEPvPmRKNS0_12DeviceHandleEm" => no_tag,
    "_ZN2nn3nfp26SetExtendedApplicationAreaERKNS0_12DeviceHandleEPKvm" => no_tag,
    "_ZN2nn3nfp30GetExtendedApplicationAreaInfoEPNS0_27ExtendedApplicationAreaInfoERKNS0_12DeviceHandleE" => no_tag,
    "_ZN2nn3nfp7RestoreERKNS0_12DeviceHandleE" => no_tag,
    "_ZN2nn3nfp13StartRestorerEPNS0_12DeviceHandleERKNS0_24AmiiboSettingsStartParamERKNS0_7TagInfoE" => no_tag,
    "_ZN2nn3nfp19StartGameDataEraserEPNS0_12DeviceHandleERKNS0_24AmiiboSettingsStartParamERKNS0_7TagInfoE" => no_tag,
    "_ZN2nn3nfp29StartNicknameAndOwnerSettingsEPNS0_12DeviceHandleEPbPNS0_12RegisterInfoERKNS0_24AmiiboSettingsStartParamERKNS0_7TagInfoE" => nickname_settings,
    "_ZN2nn3nfp29StartNicknameAndOwnerSettingsEPNS0_12DeviceHandleEPbPNS0_12RegisterInfoERKNS0_24AmiiboSettingsStartParamERKNS0_7TagInfoERKS4_" => nickname_settings,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tag_result_names_module_115() {
        assert_eq!(NO_TAG & 0x1ff, 115);
        assert_eq!(NO_TAG >> 9, 97);
    }
}
