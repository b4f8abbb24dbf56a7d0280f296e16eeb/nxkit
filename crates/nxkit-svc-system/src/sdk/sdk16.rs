//! `nn::oe`, `nn::time` and `nn::vi` take-overs of the SDK 16 file of nxnative. Source: nxnative
//! `dm/host/hle_sdk16.c`, the `oe`, `time` and `vi` sections. The `pl` and `fontll` sections are in `font.rs`, `ec` in
//! `shop.rs`, `nfc` and `nfp` in `nfp.rs`, `diag` in `diag.rs` and `settings` in `settings.rs`.
//!
//! The vi vsync event (`nn::vi::GetDisplayVsyncEvent`) is left out. The pacing crate exports it.

use super::{get_i64, nothing, put_bytes, put_f32, put_u64, put_zero, RES_FAIL};
use crate::config;
use crate::event::{self, Block};
use crate::sys;
use crate::tz;
use std::sync::OnceLock;

static START: OnceLock<u64> = OnceLock::new();
static NETWORK_TASK_EVENT: Block = Block::new();

/// Notes the start of the program. `GetProgramTotalActiveTime` counts from here.
pub(crate) fn mark_start() {
    START.get_or_init(sys::monotonic_ns);
}

/// `GetDisplayVersion(DisplayVersion*)`: 16 bytes, NUL padded.
extern "C" fn display_version(out: u64) {
    let mut b = [0u8; 16];
    let v = config().display_version.as_bytes();
    let n = v.len().min(15);
    b[..n].copy_from_slice(&v[..n]);
    put_bytes(out, &b);
}

/// `GetExpectedVolumeBalance(float*, float*)`: 1:1.
extern "C" fn get_volume_balance(system: u64, application: u64) {
    put_f32(system, 1.0);
    put_f32(application, 1.0);
}

/// `GetProgramTotalActiveTime`: `nn::TimeSpan` in nanoseconds since the program started.
extern "C" fn active_time() -> i64 {
    let start = *START.get_or_init(sys::monotonic_ns);
    i64::try_from(sys::monotonic_ns().saturating_sub(start)).unwrap_or(i64::MAX)
}

/// `ToCalendarTimeInUtc(const PosixTime&)`: `CalendarTime` in 8 bytes, `{u16 year, u8 month, day, hour, minute, second}`.
extern "C" fn calendar_in_utc(time: u64) -> u64 {
    let Some(tm) = sys::utc_time(get_i64(time)) else {
        return 0;
    };
    u64::from(tm.year as u16)
        | u64::from(tm.month as u8) << 16
        | u64::from(tm.day as u8) << 24
        | u64::from(tm.hour as u8) << 32
        | u64::from(tm.minute as u8) << 40
        | u64::from(tm.second as u8) << 48
}

/// `GetDeviceLocationName(LocationName*)`: the time zone of the host.
extern "C" fn device_location_name(out: u64) -> u32 {
    put_zero(out, tz::NAME_LEN as u64);
    put_bytes(out, &tz::location_name());
    0
}

/// `StandardNetworkSystemClock::GetCurrentTime`: the network clock is not available offline.
extern "C" fn network_now() -> u32 {
    RES_FAIL
}

/// `AsyncEnsureNetworkClockAvailabilityTask::StartTask`: no request goes out, the finish event fires at once.
extern "C" fn network_task_start(_task: u64) -> u32 {
    event::signal(NETWORK_TASK_EVENT.addr(0x100));
    0
}

/// `AsyncEnsureNetworkClockAvailabilityTask::GetFinishEvent`: the event is already signalled.
extern "C" fn network_task_event(_task: u64) -> u64 {
    let ev = NETWORK_TASK_EVENT.addr(0x100);
    event::signal(ev);
    ev
}

/// `GetIndirectImageMemoryRequirement(size_t*, size_t*, int, int)`: RGBA8 linear, 4 KB aligned.
extern "C" fn indirect_image_requirement(size: u64, align: u64, w: u32, h: u32) -> u32 {
    let (w, h) = (u64::from((w as i32).max(0) as u32), u64::from((h as i32).max(0) as u32));
    put_u64(size, (w * h * 4 + 0xfff) & !0xfff);
    put_u64(align, 0x1000);
    0
}

table! {
    "_ZN2nn2oe15SetCpuBoostModeENS0_12CpuBoostModeE" => nothing,
    "_ZN2nn2oe17GetDisplayVersionEPNS0_14DisplayVersionE" => display_version,
    "_ZN2nn2oe24GetExpectedVolumeBalanceEPfS1_" => get_volume_balance,
    "_ZN2nn2oe24SetExpectedVolumeBalanceEff" => nothing,
    "_ZN2nn2oe25GetProgramTotalActiveTimeEv" => active_time,
    "_ZN2nn2oe31EnterExitRequestHandlingSectionEv" => nothing,
    "_ZN2nn2oe31LeaveExitRequestHandlingSectionEv" => nothing,
    "_ZN2nn4time19ToCalendarTimeInUtcERKNS0_9PosixTimeE" => calendar_in_utc,
    "_ZN2nn4time21GetDeviceLocationNameEPNS0_12LocationNameE" => device_location_name,
    "_ZN2nn4time26StandardNetworkSystemClock14GetCurrentTimeEPNS0_9PosixTimeE" => network_now,
    "_ZN2nn4time8FinalizeEv" => super::ok,
    "_ZN2nn4time39AsyncEnsureNetworkClockAvailabilityTaskC1Ev" => nothing,
    "_ZN2nn4time39AsyncEnsureNetworkClockAvailabilityTaskD1Ev" => nothing,
    "_ZN2nn4time39AsyncEnsureNetworkClockAvailabilityTask10InitializeENS_2os14EventClearModeE" => super::ok,
    "_ZN2nn4time39AsyncEnsureNetworkClockAvailabilityTask9StartTaskEv" => network_task_start,
    "_ZN2nn4time39AsyncEnsureNetworkClockAvailabilityTask14GetFinishEventEv" => network_task_event,
    "_ZN2nn4time39AsyncEnsureNetworkClockAvailabilityTask9GetResultEv" => network_now,
    "_ZN2nn2vi33GetIndirectImageMemoryRequirementEPmS1_ii" => indirect_image_requirement,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_active_time_does_not_go_back() {
        mark_start();
        let a = active_time();
        assert!(active_time() >= a);
    }
}
