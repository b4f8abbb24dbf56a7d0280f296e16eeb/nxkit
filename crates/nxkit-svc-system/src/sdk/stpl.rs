//! `nn::streamplay` take-overs (GameShare and GameChat streaming of the Switch 2 SDKs). Source: The engine
//! `dm/host/hle_misc.c`, the streamplay section.
//!
//! The host streams to nobody. The library is ready and no session is open, so every guest pad, sensor and vibration
//! query finds no guest. The two settings that the game writes and reads back live in atomics, so a call takes no lock.

use super::{nothing, put_u32, put_u64, zero, zero64};
use crate::event::{self, Block};
use std::sync::atomic::{AtomicU32, Ordering};

static CONNECTION_EVENT: Block = Block::new();
static STYLES: AtomicU32 = AtomicU32::new(0);
static HOLD: AtomicU32 = AtomicU32::new(0);

/// `GetRequiredMemorySize`: one page. The host keeps no state in the work memory.
extern "C" fn required_memory_size() -> u32 {
    0x1000
}

/// `GetConnectionStateChangedEvent`: bound and never signalled.
extern "C" fn connection_event() -> u64 {
    let ev = CONNECTION_EVENT.addr(0x80);
    event::bind(ev, false);
    ev
}

/// `GetConnectionState(ConnectionState*)`: not connected.
extern "C" fn connection_state(out: u64) {
    put_u32(out, 0);
}

extern "C" fn set_styles(s: u32) {
    STYLES.store(s, Ordering::Relaxed);
}

extern "C" fn get_styles() -> u32 {
    STYLES.load(Ordering::Relaxed)
}

extern "C" fn set_hold(t: u32) {
    HOLD.store(t, Ordering::Relaxed);
}

extern "C" fn get_hold() -> u32 {
    HOLD.load(Ordering::Relaxed)
}

/// `GetGuestVibrationDeviceInfo(VibrationDeviceInfo*, handle)`: an empty record.
extern "C" fn vibration_info(info: u64) {
    put_u64(info, 0);
}

table! {
    "_ZN2nn10streamplay21GetRequiredMemorySizeENS0_14StreamPlayTypeENS0_13StreamingModeEi" => required_memory_size,
    "_ZN2nn10streamplay10InitializeENS0_14StreamPlayTypeENS0_13StreamingModeEPvm" => zero,
    "_ZN2nn10streamplay8FinalizeEv" => nothing,
    "_ZN2nn10streamplay24CreateIndividualDisplaysEi" => nothing,
    "_ZN2nn10streamplay25DestroyIndividualDisplaysEv" => nothing,
    "_ZN2nn10streamplay21GetNativeWindowHandleENS0_7GuestIdE" => zero64,
    "_ZN2nn10streamplay30GetConnectionStateChangedEventEv" => connection_event,
    "_ZN2nn10streamplay18GetConnectionStateEPNS0_15ConnectionStateE" => connection_state,
    "_ZN2nn10streamplay29SetSupportedGuestNpadStyleSetENS_4util10BitFlagSetILi32ENS_3hid12NpadStyleTagEEE" => set_styles,
    "_ZN2nn10streamplay29GetSupportedGuestNpadStyleSetEv" => get_styles,
    "_ZN2nn10streamplay23SetGuestNpadJoyHoldTypeENS_3hid15NpadJoyHoldTypeE" => set_hold,
    "_ZN2nn10streamplay23GetGuestNpadJoyHoldTypeEv" => get_hold,
    "_ZN2nn10streamplay25SetSupportedHidFeatureSetENS_4util10BitFlagSetILi32ENS0_13HidFeatureTagEEE" => nothing,
    "_ZN2nn10streamplay19GetGuestPadStyleSetERKNS0_10GuestPadIdE" => zero,
    "_ZN2nn10streamplay17GetGuestPadStatesEPNS0_13GuestPadStateINS_3hid16NpadFullKeyStateEEEiRKNS0_10GuestPadIdE" => zero,
    "_ZN2nn10streamplay17GetGuestPadStatesEPNS0_13GuestPadStateINS_3hid17NpadHandheldStateEEEiRKNS0_10GuestPadIdE" => zero,
    "_ZN2nn10streamplay17GetGuestPadStatesEPNS0_13GuestPadStateINS_3hid16NpadJoyDualStateEEEiRKNS0_10GuestPadIdE" => zero,
    "_ZN2nn10streamplay17GetGuestPadStatesEPNS0_13GuestPadStateINS_3hid17NpadJoyRightStateEEEiRKNS0_10GuestPadIdE" => zero,
    "_ZN2nn10streamplay17GetGuestPadStatesEPNS0_13GuestPadStateINS_3hid16NpadJoyLeftStateEEEiRKNS0_10GuestPadIdE" => zero,
    "_ZN2nn10streamplay28GetGuestSixAxisSensorHandlesEPNS0_24GuestSixAxisSensorHandleEiRKNS0_10GuestPadIdENS_4util10BitFlagSetILi32ENS_3hid12NpadStyleTagEEE" => zero,
    "_ZN2nn10streamplay27GetGuestSixAxisSensorStatesEPNS0_23GuestSixAxisSensorStateEiRKNS0_24GuestSixAxisSensorHandleE" => zero,
    "_ZN2nn10streamplay30GetGuestVibrationDeviceHandlesEPNS0_26GuestVibrationDeviceHandleEiRKNS0_10GuestPadIdENS_4util10BitFlagSetILi32ENS_3hid12NpadStyleTagEEE" => zero,
    "_ZN2nn10streamplay27GetGuestVibrationDeviceInfoEPNS_3hid19VibrationDeviceInfoERKNS0_26GuestVibrationDeviceHandleE" => vibration_info,
    "_ZN2nn10streamplay18GetGuestJcmsHandleERKNS0_10GuestPadIdERKNS_3hid8JcmsTypeE" => zero,
    "_ZN2nn10streamplay18GetGuestJcmsStatesEPNS0_14GuestJcmsStateEiRKNS0_15GuestJcmsHandleE" => zero,
}
