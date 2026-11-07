//! The libc calls of the crate: the monotonic clock and the local time of the host.
//!
//! This module is the only one of the crate that contains `unsafe`. Every block is one call with arguments that are
//! valid for it.
#![allow(unsafe_code)]

use std::ffi::{CStr, c_char, c_int, c_long};

#[repr(C)]
struct Timespec {
    sec: i64,
    nsec: c_long,
}

/// `struct tm` of macOS and glibc.
#[repr(C)]
struct RawTm {
    sec: c_int,
    min: c_int,
    hour: c_int,
    mday: c_int,
    mon: c_int,
    year: c_int,
    wday: c_int,
    yday: c_int,
    isdst: c_int,
    gmtoff: c_long,
    zone: *const c_char,
}

#[cfg(target_os = "macos")]
const CLOCK_MONOTONIC: c_int = 6;
#[cfg(not(target_os = "macos"))]
const CLOCK_MONOTONIC: c_int = 1;

unsafe extern "C" {
    fn clock_gettime(clock: c_int, ts: *mut Timespec) -> c_int;
    fn localtime_r(t: *const i64, out: *mut RawTm) -> *mut RawTm;
    fn mktime(tm: *mut RawTm) -> i64;
}

impl RawTm {
    fn empty() -> RawTm {
        RawTm {
            sec: 0,
            min: 0,
            hour: 0,
            mday: 0,
            mon: 0,
            year: 0,
            wday: 0,
            yday: 0,
            isdst: 0,
            gmtoff: 0,
            zone: std::ptr::null(),
        }
    }
}

/// A broken-down local time.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Tm {
    /// The full year, for example 2026.
    pub(crate) year: i32,
    /// 1 to 12.
    pub(crate) month: u32,
    /// 1 to 31.
    pub(crate) day: u32,
    pub(crate) hour: u32,
    pub(crate) minute: u32,
    pub(crate) second: u32,
    /// Days since Sunday.
    pub(crate) wday: u32,
    /// Days since 1 January.
    pub(crate) yday: u32,
    /// Daylight saving time is in effect.
    pub(crate) dst: bool,
    /// Seconds east of UTC.
    pub(crate) offset: i32,
    /// The abbreviation of the zone, for example `CET`.
    pub(crate) zone: String,
}

/// The monotonic clock of the host in nanoseconds.
pub(crate) fn monotonic_ns() -> u64 {
    let mut ts = Timespec { sec: 0, nsec: 0 };
    // SAFETY: `ts` is valid for writes and `clock_gettime` writes only to it.
    let rc = unsafe { clock_gettime(CLOCK_MONOTONIC, &mut ts) };
    if rc != 0 {
        return 0;
    }
    u64::try_from(ts.sec)
        .unwrap_or(0)
        .saturating_mul(1_000_000_000)
        .saturating_add(u64::try_from(ts.nsec).unwrap_or(0))
}

fn zone_name(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: after a successful `localtime_r`, `tm_zone` points to a NUL-terminated name in static storage of the C
    // library.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

fn field(v: c_int) -> u32 {
    u32::try_from(v).unwrap_or(0)
}

/// The local time of the host at the POSIX time `t`.
pub(crate) fn local_time(t: i64) -> Option<Tm> {
    let mut raw = RawTm::empty();
    // SAFETY: `t` and `raw` are valid for the call and `localtime_r` writes only to `raw`.
    let p = unsafe { localtime_r(&t, &mut raw) };
    if p.is_null() {
        return None;
    }
    Some(Tm {
        year: raw.year.checked_add(1900)?,
        month: field(raw.mon) + 1,
        day: field(raw.mday),
        hour: field(raw.hour),
        minute: field(raw.min),
        second: field(raw.sec),
        wday: field(raw.wday),
        yday: field(raw.yday),
        dst: raw.isdst > 0,
        offset: i32::try_from(raw.gmtoff).unwrap_or(0),
        zone: zone_name(raw.zone),
    })
}

/// The POSIX time of a local time of the host. The host decides whether daylight saving time is in effect.
pub(crate) fn to_posix(tm: &Tm) -> Option<i64> {
    let mut raw = RawTm::empty();
    raw.year = tm.year.checked_sub(1900)?;
    raw.mon = c_int::try_from(tm.month).ok()?.checked_sub(1)?;
    raw.mday = c_int::try_from(tm.day).ok()?;
    raw.hour = c_int::try_from(tm.hour).ok()?;
    raw.min = c_int::try_from(tm.minute).ok()?;
    raw.sec = c_int::try_from(tm.second).ok()?;
    raw.isdst = -1;
    // SAFETY: `raw` is valid for the call and `mktime` reads and normalizes only it.
    let t = unsafe { mktime(&mut raw) };
    (t != -1).then_some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_monotonic_clock_does_not_go_back() {
        let a = monotonic_ns();
        let b = monotonic_ns();
        assert!(a > 0 && b >= a);
    }

    #[test]
    fn local_time_round_trips() {
        let t = 1_700_000_000;
        let tm = local_time(t);
        assert_eq!(tm.as_ref().and_then(to_posix), Some(t));
    }
}
