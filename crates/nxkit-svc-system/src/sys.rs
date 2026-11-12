//! The libc calls of the crate (the monotonic clock and the time of the host) and the calls into guest code.
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

/// `struct tm` of Darwin (macOS and iOS) and glibc.
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

#[cfg(any(target_os = "macos", target_os = "ios"))]
const CLOCK_MONOTONIC: c_int = 6;
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
const CLOCK_MONOTONIC: c_int = 1;

// The shared Darwin declarations must match the iPhone libc ABI. Check the
// complete layout during every iOS build, not only when tests run on a device.
#[cfg(target_os = "ios")]
const _: () = {
    use std::mem::{align_of, offset_of, size_of};
    assert!(size_of::<i64>() == size_of::<libc::time_t>());
    assert!(size_of::<Timespec>() == size_of::<libc::timespec>());
    assert!(align_of::<Timespec>() == align_of::<libc::timespec>());
    assert!(offset_of!(Timespec, sec) == offset_of!(libc::timespec, tv_sec));
    assert!(offset_of!(Timespec, nsec) == offset_of!(libc::timespec, tv_nsec));
    assert!(size_of::<RawTm>() == size_of::<libc::tm>());
    assert!(align_of::<RawTm>() == align_of::<libc::tm>());
    assert!(offset_of!(RawTm, sec) == offset_of!(libc::tm, tm_sec));
    assert!(offset_of!(RawTm, min) == offset_of!(libc::tm, tm_min));
    assert!(offset_of!(RawTm, hour) == offset_of!(libc::tm, tm_hour));
    assert!(offset_of!(RawTm, mday) == offset_of!(libc::tm, tm_mday));
    assert!(offset_of!(RawTm, mon) == offset_of!(libc::tm, tm_mon));
    assert!(offset_of!(RawTm, year) == offset_of!(libc::tm, tm_year));
    assert!(offset_of!(RawTm, wday) == offset_of!(libc::tm, tm_wday));
    assert!(offset_of!(RawTm, yday) == offset_of!(libc::tm, tm_yday));
    assert!(offset_of!(RawTm, isdst) == offset_of!(libc::tm, tm_isdst));
    assert!(offset_of!(RawTm, gmtoff) == offset_of!(libc::tm, tm_gmtoff));
    assert!(offset_of!(RawTm, zone) == offset_of!(libc::tm, tm_zone));
    // Darwin libc exposes clockid_t as u32; compare both 32-bit values losslessly.
    assert!(CLOCK_MONOTONIC as i64 == libc::CLOCK_MONOTONIC as i64);
};

unsafe extern "C" {
    fn clock_gettime(clock: c_int, ts: *mut Timespec) -> c_int;
    fn localtime_r(t: *const i64, out: *mut RawTm) -> *mut RawTm;
    fn gmtime_r(t: *const i64, out: *mut RawTm) -> *mut RawTm;
    fn mktime(tm: *mut RawTm) -> i64;
    fn timegm(tm: *mut RawTm) -> i64;
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
    /// The abbreviation of the zone, for example `CET`, NUL padded. Longer names are cut to 7 bytes.
    pub(crate) zone: [u8; 8],
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

fn zone_name(p: *const c_char) -> [u8; 8] {
    let mut out = [0u8; 8];
    if p.is_null() {
        return out;
    }
    // SAFETY: after a successful `localtime_r`, `tm_zone` points to a NUL-terminated name in static storage of the C
    // library.
    let name = unsafe { CStr::from_ptr(p) }.to_bytes();
    let n = name.len().min(7);
    out[..n].copy_from_slice(&name[..n]);
    out
}

#[cfg(not(target_os = "ios"))]
fn field(v: c_int) -> u32 {
    u32::try_from(v).unwrap_or(0)
}

#[cfg(not(target_os = "ios"))]
fn convert(raw: &RawTm) -> Option<Tm> {
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

// Retain the native app boundary's checked conversion of libc fields.
#[cfg(target_os = "ios")]
fn convert(raw: &RawTm) -> Option<Tm> {
    Some(Tm {
        year: raw.year.checked_add(1900)?,
        month: (raw.mon + 1).try_into().ok()?,
        day: raw.mday.try_into().ok()?,
        hour: raw.hour.try_into().ok()?,
        minute: raw.min.try_into().ok()?,
        second: raw.sec.try_into().ok()?,
        wday: raw.wday.try_into().ok()?,
        yday: raw.yday.try_into().ok()?,
        dst: raw.isdst > 0,
        offset: raw.gmtoff.try_into().ok()?,
        zone: zone_name(raw.zone),
    })
}

fn raw_from(tm: &Tm) -> Option<RawTm> {
    let mut raw = RawTm::empty();
    raw.year = tm.year.checked_sub(1900)?;
    raw.mon = c_int::try_from(tm.month).ok()?.checked_sub(1)?;
    raw.mday = c_int::try_from(tm.day).ok()?;
    raw.hour = c_int::try_from(tm.hour).ok()?;
    raw.min = c_int::try_from(tm.minute).ok()?;
    raw.sec = c_int::try_from(tm.second).ok()?;
    raw.isdst = -1;
    Some(raw)
}

/// The local time of the host at the POSIX time `t`.
pub(crate) fn local_time(t: i64) -> Option<Tm> {
    let mut raw = RawTm::empty();
    // SAFETY: `t` and `raw` are valid for the call and `localtime_r` writes only to `raw`.
    let p = unsafe { localtime_r(&t, &mut raw) };
    if p.is_null() {
        return None;
    }
    convert(&raw)
}

/// The UTC time at the POSIX time `t`.
pub(crate) fn utc_time(t: i64) -> Option<Tm> {
    let mut raw = RawTm::empty();
    // SAFETY: `t` and `raw` are valid for the call and `gmtime_r` writes only to `raw`.
    let p = unsafe { gmtime_r(&t, &mut raw) };
    if p.is_null() {
        return None;
    }
    convert(&raw)
}

/// The POSIX time of a local time of the host. The host decides whether daylight saving time is in effect.
pub(crate) fn to_posix(tm: &Tm) -> Option<i64> {
    let mut raw = raw_from(tm)?;
    // SAFETY: `raw` is valid for the call and `mktime` reads and normalizes only it.
    let t = unsafe { mktime(&mut raw) };
    (t != -1).then_some(t)
}

/// The POSIX time of a UTC time.
pub(crate) fn to_posix_utc(tm: &Tm) -> Option<i64> {
    let mut raw = raw_from(tm)?;
    // SAFETY: `raw` is valid for the call and `timegm` reads and normalizes only it.
    let t = unsafe { timegm(&mut raw) };
    (t != -1).then_some(t)
}

/// The POSIX time of the nine `int` fields of a guest `struct tm` (second, minute, hour, day of month, month, year since
/// 1900, weekday, day of year, daylight saving flag) in the zone of the host. Out of range fields are normalized, as
/// `mktime` does it.
pub(crate) fn mktime_fields(f: [i32; 9]) -> Option<i64> {
    let mut raw = RawTm::empty();
    raw.sec = f[0];
    raw.min = f[1];
    raw.hour = f[2];
    raw.mday = f[3];
    raw.mon = f[4];
    raw.year = f[5];
    raw.isdst = f[8];
    // SAFETY: `raw` is valid for the call and `mktime` reads and normalizes only it.
    let t = unsafe { mktime(&mut raw) };
    (t != -1).then_some(t)
}

/// Calls the guest function at `addr` with up to six integer arguments and gives its integer result.
///
/// The address comes from the `guest_sym` seam, which resolves a symbol of the game. A function with fewer
/// arguments ignores the rest, and a function that returns nothing leaves a value that the caller ignores. The call
/// must run on a guest thread when the function needs the guest runtime.
#[cfg(not(target_os = "ios"))]
pub(crate) fn call_guest(addr: u64, args: [u64; 6]) -> u64 {
    type Guest = extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64;
    let Ok(addr) = usize::try_from(addr) else {
        return 0;
    };
    // SAFETY: `addr` is the entry of a function that the host resolved in the loaded game. The guest code is native
    // code of this process, and its calling convention is the C one.
    let f: Guest = unsafe { std::mem::transmute::<usize, Guest>(addr) };
    f(args[0], args[1], args[2], args[3], args[4], args[5])
}

#[cfg(target_os = "ios")]
pub(crate) fn call_guest(addr: u64, args: [u64; 6]) -> u64 {
    call_guest8(addr, args, [0; 2])
}

/// Calls the guest function at `addr` with seven integer arguments.
pub(crate) fn call_guest7(addr: u64, args: [u64; 6], seventh: u64) -> u64 {
    call_guest8(addr, args, [seventh, 0])
}

/// Calls the guest function at `addr` with eight integer arguments. The first eight go in registers.
pub(crate) fn call_guest8(addr: u64, args: [u64; 6], more: [u64; 2]) -> u64 {
    type Guest = extern "C" fn(u64, u64, u64, u64, u64, u64, u64, u64) -> u64;
    let Ok(addr) = usize::try_from(addr) else {
        return 0;
    };
    #[cfg(target_os = "ios")]
    if addr == 0 {
        return 0;
    }
    // SAFETY: as in `call_guest`. The two extra arguments fill the seventh and eighth register.
    let f: Guest = unsafe { std::mem::transmute::<usize, Guest>(addr) };
    f(
        args[0], args[1], args[2], args[3], args[4], args[5], more[0], more[1],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "ios")]
    #[test]
    fn null_guest_is_not_callable() {
        assert_eq!(call_guest(0, [0; 6]), 0);
        assert_eq!(call_guest7(0, [0; 6], 0), 0);
        assert_eq!(call_guest8(0, [0; 6], [0; 2]), 0);
    }

    #[test]
    fn the_monotonic_clock_does_not_go_back() {
        let a = monotonic_ns();
        let b = monotonic_ns();
        assert!(a > 0 && b >= a);
    }

    #[test]
    fn utc_time_round_trips() {
        let t = 1_700_000_000;
        let tm = utc_time(t);
        assert_eq!(tm.as_ref().map(|t| t.year), Some(2023));
        assert_eq!(tm.as_ref().and_then(to_posix_utc), Some(t));
    }

    #[test]
    fn local_time_round_trips() {
        let t = 1_700_000_000;
        let tm = local_time(t);
        assert_eq!(tm.as_ref().and_then(to_posix), Some(t));
    }
}
