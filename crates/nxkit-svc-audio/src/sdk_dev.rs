//! The SDK take-overs of the audio device functions of `hle_audren.c`: names, channel count, volume and notifications.
//! The host has one built-in speaker with two channels.

use crate::host;
use crate::util::put_name;
use crate::util::ex;
use nxkit_abi::export::Export;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

pub(crate) const DEVNAME: usize = 0x100;
pub(crate) const SPEAKER: &str = "AudioBuiltInSpeakerOutput";
/// The result of a notification that was acquired before.
const ALREADY_ACQUIRED: u32 = 0x499;

/// The volume of the device as float bits, shared with the IPC `IAudioDevice`.
pub(crate) static DEV_VOL: AtomicU32 = AtomicU32::new(0x3f80_0000);
/// The auto-tune flag, shared with the IPC `IAudioDevice`.
pub(crate) static AUTOTUNE: AtomicBool = AtomicBool::new(false);
static NOTIFY_OUT: AtomicBool = AtomicBool::new(false);
static NOTIFY_IN: AtomicBool = AtomicBool::new(false);

extern "C" fn list_names(names: u64, n: i32) -> i32 {
    if n <= 0 || names == 0 {
        return 0;
    }
    put_name(names, SPEAKER, DEVNAME);
    1
}

extern "C" fn active_name(name: u64) {
    put_name(name, SPEAKER, DEVNAME);
}

extern "C" fn channels() -> i32 {
    2
}

extern "C" fn set_volume(_name: u64, v: f32) {
    DEV_VOL.store(v.to_bits(), Ordering::Relaxed);
}

extern "C" fn get_volume(_name: u64) -> f32 {
    f32::from_bits(DEV_VOL.load(Ordering::Relaxed))
}

extern "C" fn set_autotune(on: u32) {
    AUTOTUNE.store(on & 1 != 0, Ordering::Relaxed);
}

extern "C" fn is_autotune() -> u32 {
    u32::from(AUTOTUNE.load(Ordering::Relaxed))
}

/// Returns 0, or `ALREADY_ACQUIRED` when the flag was set. It binds the event only the first time.
fn acquire(flag: &AtomicBool, ev: u64) -> u32 {
    if flag.swap(true, Ordering::AcqRel) {
        return ALREADY_ACQUIRED;
    }
    host::bind_event(ev, false);
    0
}

extern "C" fn notify_output(ev: u64) -> u32 {
    acquire(&NOTIFY_OUT, ev)
}

extern "C" fn notify_input(ev: u64) -> u32 {
    acquire(&NOTIFY_IN, ev)
}

extern "C" fn notify_switch(ev: u64) {
    host::bind_event(ev, false);
}

/// Adds the exports of this module.
pub(crate) fn exports(v: &mut Vec<Export>) {
    ex!(v, "_ZN2nn5audio25ListAudioOutputDeviceNameEPNS0_15AudioDeviceNameEi", list_names);
    ex!(v, "_ZN2nn5audio30GetActiveAudioOutputDeviceNameEPNS0_15AudioDeviceNameE", active_name);
    ex!(v, "_ZN2nn5audio19ListAudioDeviceNameEPNS0_15AudioDeviceNameEi", list_names);
    ex!(v, "_ZN2nn5audio24GetActiveAudioDeviceNameEPNS0_15AudioDeviceNameE", active_name);
    ex!(v, "_ZN2nn5audio41GetActiveAudioDeviceChannelCountForOutputEv", channels);
    ex!(v, "_ZN2nn5audio26SetAudioDeviceOutputVolumeEPKNS0_15AudioDeviceNameEf", set_volume);
    ex!(v, "_ZN2nn5audio26GetAudioDeviceOutputVolumeEPKNS0_15AudioDeviceNameE", get_volume);
    ex!(v, "_ZN2nn5audio40SetDeviceOutputVolumeAutoTuneModeEnabledEb", set_autotune);
    ex!(v, "_ZN2nn5audio39IsDeviceOutputVolumeAutoTuneModeEnabledEv", is_autotune);
    ex!(v, "_ZN2nn5audio39AcquireAudioDeviceNotificationForOutputEPNS_2os11SystemEventE", notify_output);
    ex!(v, "_ZN2nn5audio36AcquireAudioOutputDeviceNotificationEPNS_2os11SystemEventE", notify_output);
    ex!(v, "_ZN2nn5audio38AcquireAudioDeviceNotificationForInputEPNS_2os11SystemEventE", notify_input);
    ex!(v, "_ZN2nn5audio36AcquireAudioDeviceSwitchNotificationEPNS_2os11SystemEventE", notify_switch);
    ex!(v, "_ZN2nn5audio21GetActiveChannelCountEv", channels);
}
