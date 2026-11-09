//! `nn::time` take-overs and the C library time functions.
//!
//! The clocks come from the host. `Config::fixed_time` makes the user clock return one value. The
//! calendar functions follow the time zone of the host. The libc `localtime`, `localtime_r` and `mktime` are forced
//! onto the host zone too, because the SDK copies have no zone data and always give UTC. `gmtime` and `timegm` are
//! UTC in the SDK as well and stay with it.

use super::{
    get_i32, get_i64, get_u8, get_u16, log, log_once_with, ok, one, put_bytes, put_i32, put_i64,
    put_u32, put_zero, zero,
};
use crate::config;
use crate::event::Block;
use crate::seams::guest_ns;
use crate::sys;
use crate::time::{self as clock, SNAPSHOT_LEN, snapshot};
use crate::tz;
use crate::util::lock;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static CALENDAR_LOGGED: AtomicBool = AtomicBool::new(false);
static CONTEXT: Mutex<Option<([u8; 0x20], i64)>> = Mutex::new(None);
static TM_STATIC: Block = Block::new();
static ZONES: Block = Block::new();
static ZONE_KEYS: [AtomicU64; ZONE_SLOTS as usize] =
    [const { AtomicU64::new(0) }; ZONE_SLOTS as usize];

/// The size of a guest `struct tm` and the place of its fields: nine `int`, then `long tm_gmtoff` at 40 and
/// `const char* tm_zone` at 48.
const TM_SIZE: u64 = 56;
const ZONE_SLOT: u64 = 16;
const ZONE_SLOTS: u64 = 64;

/// The time of the user clock for a call. A fixed time does not advance.
fn user_time() -> i64 {
    config().fixed_time.unwrap_or_else(clock::real_now)
}

fn put_steady(out: u64) -> u32 {
    put_bytes(out, &clock::steady_point());
    0
}

/// `StandardSteadyClock::GetCurrentTimePoint` and `GetStandardSteadyClockCurrentTimePoint`.
extern "C" fn steady_now(out: u64) -> u32 {
    put_steady(out)
}

/// `StandardUserSystemClock::GetCurrentTime` and the adjustable clock.
extern "C" fn current_time(out: u64) -> u32 {
    put_i64(out, user_time());
    0
}

/// `StandardUserSystemClock::now()`.
extern "C" fn now() -> i64 {
    user_time()
}

/// `StandardUserSystemClock::to_time_t(const time_point&)`.
extern "C" fn to_time_t(point: u64) -> i64 {
    get_i64(point)
}

fn put_timespec(ts: u64, sec: i64, nsec: i64) {
    put_i64(ts, sec);
    put_i64(ts + 8, nsec);
}

/// The C library real-time clock from the user clock. SDK 17 reads it through these functions.
extern "C" fn user_timespec(ts: u64) -> u32 {
    match config().fixed_time {
        Some(t) => put_timespec(ts, t, 0),
        None => {
            let d = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            put_timespec(
                ts,
                i64::try_from(d.as_secs()).unwrap_or(0),
                i64::from(d.subsec_nanos()),
            );
        }
    }
    0
}

/// The C library monotonic clock from the steady clock. SDK 22 reads it through this function.
extern "C" fn steady_timespec(ts: u64) -> u32 {
    let ns = sys::monotonic_ns();
    put_timespec(
        ts,
        i64::try_from(ns / 1_000_000_000).unwrap_or(0),
        i64::try_from(ns % 1_000_000_000).unwrap_or(0),
    );
    0
}

/// The user clock context: made once, renewed when the host clock moves against the guest ticks by more than a second.
fn user_context() -> [u8; 0x20] {
    let now = user_time();
    let drift = now - i64::try_from(guest_ns() / 1_000_000_000).unwrap_or(0);
    let mut g = lock(&CONTEXT);
    let renew = match *g {
        None => true,
        Some((_, old)) => config().fixed_time.is_none() && (drift - old).abs() > 1,
    };
    if renew {
        let point = clock::steady_point();
        let offset = now
            - i64::from_le_bytes([
                point[0], point[1], point[2], point[3], point[4], point[5], point[6], point[7],
            ]);
        let mut ctx = [0u8; 0x20];
        ctx[..8].copy_from_slice(&offset.to_le_bytes());
        ctx[8..].copy_from_slice(&point);
        if let Some((_, old)) = *g {
            log(&format!(
                "time: host clock moved {:+} s against the guest ticks, new user clock context",
                drift - old
            ));
        }
        *g = Some((ctx, drift));
    }
    g.map_or([0u8; 0x20], |(ctx, _)| ctx)
}

/// `StandardUserSystemClock::GetSystemClockContext(SystemClockContext*)`.
extern "C" fn system_clock_context(out: u64) -> u32 {
    put_bytes(out, &user_context());
    0
}

fn put_calendar(ct: u64, ai: u64, posix: i64) {
    let Some(cal) = tz::calendar(posix) else {
        return;
    };
    log_once_with(&CALENDAR_LOGGED, || {
        format!(
            "ToCalendarTime: first conversion {posix} gives year {}",
            u16::from_le_bytes([cal[0], cal[1]])
        )
    });
    put_bytes(ct, &cal[..8]);
    put_bytes(ai, &cal[8..]);
}

/// `ToCalendarTime(CalendarTime*, CalendarAdditionalInfo*, const PosixTime&)`, with or without a zone rule.
extern "C" fn to_calendar_time(ct: u64, ai: u64, posix: u64) -> u32 {
    put_calendar(ct, ai, get_i64(posix));
    0
}

/// `LoadTimeZoneRule(TimeZoneRule*, const LocationName&)`: the rule stays unused, the host converts with its zone.
extern "C" fn load_time_zone_rule(_rule: u64, name: u64) -> u32 {
    static LOGGED: AtomicBool = AtomicBool::new(false);
    log_once_with(&LOGGED, || {
        format!(
            "time::LoadTimeZoneRule {} (host time zone used instead)",
            super::get_cstr(name, 36)
        )
    });
    0
}

/// `ClockSnapshot::CreateWithStandardSystemClock(ClockSnapshot*)`.
fn fill_snapshot(out: u64, clock_type: u8) -> u32 {
    let mut b = snapshot(clock_type).unwrap_or([0u8; SNAPSHOT_LEN]);
    b[..0x20].copy_from_slice(&user_context());
    put_bytes(out, &b);
    0
}

extern "C" fn snapshot_standard(out: u64) -> u32 {
    fill_snapshot(out, 1)
}

extern "C" fn snapshot_adjustable(out: u64) -> u32 {
    fill_snapshot(out, 2)
}

fn snap_i64(addr: u64, offset: u64) -> i64 {
    get_i64(addr + offset)
}

/// True when two steady time points at `offset` have the same source id.
fn same_source(a: u64, b: u64, offset: u64) -> bool {
    super::get_bytes(a + offset + 8, 16) == super::get_bytes(b + offset + 8, 16)
}

/// `CalculateSpanBetween(TimeSpan*, const ClockSnapshot&, const ClockSnapshot&)`: gives a `bool`.
extern "C" fn span_between(out: u64, a: u64, b: u64) -> u32 {
    let seconds = if same_source(a, b, 0x90) {
        snap_i64(b, 0x90) - snap_i64(a, 0x90)
    } else if snap_i64(a, 0x48) != 0 && snap_i64(b, 0x48) != 0 {
        snap_i64(b, 0x48) - snap_i64(a, 0x48)
    } else {
        return 0;
    };
    put_i64(out, seconds.saturating_mul(1_000_000_000));
    1
}

/// `CalculateStandardUserSystemClockDifferenceByUser`: the change of the user clock offset in nanoseconds.
extern "C" fn user_difference(a: u64, b: u64) -> i64 {
    if !same_source(a, b, 0x08) || (get_u8(a + 0xcc) != 0 && get_u8(b + 0xcc) != 0) {
        return 0;
    }
    (snap_i64(b, 0) - snap_i64(a, 0)).saturating_mul(1_000_000_000)
}

/// The fields of a guest `CalendarTime`.
fn calendar_bytes(ct: u64) -> [u8; 7] {
    let year = get_u16(ct).to_le_bytes();
    [
        year[0],
        year[1],
        get_u8(ct + 2),
        get_u8(ct + 3),
        get_u8(ct + 4),
        get_u8(ct + 5),
        get_u8(ct + 6),
    ]
}

/// `ToPosixTime(int*, PosixTime*, int, const CalendarTime&)`, with or without a zone rule: one candidate.
extern "C" fn to_posix_time(count: u64, out: u64, n: u32, ct: u64) -> u32 {
    let t = tz::posix(&calendar_bytes(ct)).unwrap_or(-1);
    put_u32(count, u32::from(n as i32 > 0));
    if n as i32 > 0 {
        put_i64(out, t);
    }
    0
}

/// `ToPosixTimeFromUtc(const CalendarTime&)`.
extern "C" fn to_posix_from_utc(ct: u64) -> i64 {
    let b = calendar_bytes(ct);
    sys::to_posix_utc(&sys::Tm {
        year: i32::from(u16::from_le_bytes([b[0], b[1]])),
        month: u32::from(b[2]),
        day: u32::from(b[3]),
        hour: u32::from(b[4]),
        minute: u32::from(b[5]),
        second: u32::from(b[6]),
        ..sys::Tm::default()
    })
    .unwrap_or(-1)
}

/// The guest address of the name of a zone, stable for the life of the process. The 64 slots are claimed with one
/// atomic exchange each, so a lookup takes no lock and makes no allocation. A reader that races the first writer of a
/// name can see an unwritten slot for one call of the first conversion.
fn zone_address(zone: [u8; 8]) -> u64 {
    let base = ZONES.addr(ZONE_SLOT * ZONE_SLOTS);
    if base == 0 {
        return 0;
    }
    // The last byte of a name is always NUL, so the marker bit keeps the key of an empty name from reading as a free slot.
    let key = u64::from_le_bytes(zone) | (1 << 56);
    for (slot, owner) in ZONE_KEYS.iter().enumerate() {
        let at = base + slot as u64 * ZONE_SLOT;
        match owner.compare_exchange(0, key, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => {
                put_bytes(at, &zone);
                return at;
            }
            Err(held) if held == key => return at,
            Err(_) => {}
        }
    }
    base
}

/// Writes the local time of `posix` into the guest `struct tm` at `out`.
fn put_tm(out: u64, posix: i64) -> u64 {
    let Some(tm) = sys::local_time(posix) else {
        return 0;
    };
    put_zero(out, TM_SIZE);
    let fields = [
        tm.second as i32,
        tm.minute as i32,
        tm.hour as i32,
        tm.day as i32,
        tm.month as i32 - 1,
        tm.year - 1900,
        tm.wday as i32,
        tm.yday as i32,
        i32::from(tm.dst),
    ];
    for (i, f) in fields.iter().enumerate() {
        put_i32(out + 4 * i as u64, *f);
    }
    put_i64(out + 40, i64::from(tm.offset));
    put_i64(out + 48, zone_address(tm.zone) as i64);
    out
}

/// `localtime(const time_t*)`: the result is a static `struct tm`.
extern "C" fn localtime(t: u64) -> u64 {
    let out = TM_STATIC.addr(0x1000);
    if out == 0 {
        return 0;
    }
    put_tm(out, get_i64(t))
}

/// `localtime_r(const time_t*, struct tm*)`.
extern "C" fn localtime_r(t: u64, out: u64) -> u64 {
    put_tm(out, get_i64(t))
}

/// `mktime(struct tm*)`: the fields are normalized in place.
extern "C" fn mktime(tm: u64) -> i64 {
    let mut f = [0i32; 9];
    for (i, v) in f.iter_mut().enumerate() {
        *v = get_i32(tm + 4 * i as u64);
    }
    let Some(t) = sys::mktime_fields(f) else {
        return -1;
    };
    let zone = get_i64(tm + 48);
    put_tm(tm, t);
    if zone != 0 && get_i64(tm + 48) == 0 {
        put_i64(tm + 48, zone);
    }
    t
}

table! {
    "_ZN2nn4time10InitializeEv" => ok,
    "_ZN2nn4time13IsInitializedEv" => one,
    "_ZN2nn4time19StandardSteadyClock19GetCurrentTimePointEPNS0_20SteadyClockTimePointE" => steady_now,
    "_ZN2nn4time38GetStandardSteadyClockCurrentTimePointEPNS0_20SteadyClockTimePointE" => steady_now,
    "_ZN2nn4time23StandardUserSystemClock14GetCurrentTimeEPNS0_9PosixTimeE" => current_time,
    "_ZN2nn4time25AdjustableUserSystemClock14GetCurrentTimeEPNS0_9PosixTimeE" => current_time,
    "_ZN2nn4time41GetStandardUserSystemClockCurrentTimespecEP8timespec" => user_timespec,
    "_ZN2nn4time43GetAdjustableUserSystemClockCurrentTimespecEP8timespec" => user_timespec,
    "_ZN2nn4time37GetStandardSteadyClockCurrentTimespecEP8timespec" => steady_timespec,
    "_ZN2nn4time23StandardUserSystemClock21GetSystemClockContextEPNS0_18SystemClockContextE" => system_clock_context,
    "_ZN2nn4time23StandardUserSystemClock3nowEv" => now,
    "_ZN2nn4time23StandardUserSystemClock9to_time_tERKNSt3__16chrono10time_pointINS0_17SystemClockTraitsENS3_8durationIlNS2_5ratioILl1ELl1EEEEEEE" => to_time_t,
    "_ZN2nn4time14ToCalendarTimeEPNS0_12CalendarTimeEPNS0_22CalendarAdditionalInfoERKNS0_9PosixTimeE" => to_calendar_time,
    "_ZN2nn4time16LoadTimeZoneRuleEPNS0_12TimeZoneRuleERKNS0_12LocationNameE" => load_time_zone_rule,
    "_ZN2nn4time14ToCalendarTimeEPNS0_12CalendarTimeEPNS0_22CalendarAdditionalInfoERKNS0_9PosixTimeERKNS0_12TimeZoneRuleE" => to_calendar_time,
    "_ZN2nn4time26StandardNetworkSystemClock20IsAccuracySufficientEv" => zero,
    "_ZN2nn4time13ClockSnapshot29CreateWithStandardSystemClockEPS1_" => snapshot_standard,
    "_ZN2nn4time13ClockSnapshot31CreateWithAdjustableSystemClockEPS1_" => snapshot_adjustable,
    "_ZN2nn4time20CalculateSpanBetweenEPNS_8TimeSpanERKNS0_13ClockSnapshotES5_" => span_between,
    "_ZN2nn4time48CalculateStandardUserSystemClockDifferenceByUserERKNS0_13ClockSnapshotES3_" => user_difference,
    "_ZN2nn4time11ToPosixTimeEPiPNS0_9PosixTimeEiRKNS0_12CalendarTimeE" => to_posix_time,
    "_ZN2nn4time11ToPosixTimeEPiPNS0_9PosixTimeEiRKNS0_12CalendarTimeERKNS0_12TimeZoneRuleE" => to_posix_time,
    "_ZN2nn4time18ToPosixTimeFromUtcERKNS0_12CalendarTimeE" => to_posix_from_utc,
    "localtime" => localtime,
    "localtime_r" => localtime_r,
    "mktime" => mktime,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_user_clock_context_stays_between_calls() {
        let a = user_context();
        assert_eq!(user_context(), a);
    }
}
