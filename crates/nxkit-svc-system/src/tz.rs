//! `ITimeZoneService` of the `time:*` services, and the calendar conversion behind it.
//!
//! Commands (IDs of switchbrew `Time services`, behavior of the engine `dm/host/hle_time.c`):
//! - 0 `GetDeviceLocationName`, 2 `GetTotalLocationNameCount`, 3 `LoadLocationNameList`: the one location of the host.
//! - 4 `LoadTimeZoneRule`: leaves the rule buffer untouched. The conversion below never reads it.
//! - 100 `ToCalendarTime`, 101 `ToCalendarTimeWithMyRule`, 201 `ToPosixTime`, 202 `ToPosixTimeWithMyRule`: convert with
//!   the local time of the host (`localtime_r`, `mktime`).
//!
//! Command 1 `SetDeviceLocationName` is unknown: a game cannot set the zone of the host.

use crate::sys::{self, Tm};
use crate::util::{RES_FAIL, fail, out_buf, padded, write_out};
use nxkit_abi::ipc::{Handled, Request, Response, Service};
use std::sync::OnceLock;

/// The size of a location name, `nn::time::LocationName`.
pub(crate) const NAME_LEN: usize = 0x24;

/// The size of the pair `CalendarTime` and `CalendarAdditionalInfo` that the conversion returns.
pub(crate) const CALENDAR_LEN: usize = 0x20;

/// The name of the zone of the host, for example `Europe/Berlin`.
///
/// The name comes from `TZ`, else from the target of the link `/etc/localtime`, else it is `UTC`. The engine gives the
/// abbreviation `tzname[0]` here, which is not a location name.
fn name_text() -> &'static str {
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| {
        if let Ok(tz) = std::env::var("TZ") {
            let tz = tz.trim_start_matches(':');
            if !tz.is_empty() && !tz.starts_with('/') {
                return tz.to_owned();
            }
        }
        if let Ok(path) = std::fs::read_link("/etc/localtime") {
            let path = path.to_string_lossy();
            if let Some(i) = path.find("zoneinfo/") {
                return path[i + "zoneinfo/".len()..].to_owned();
            }
        }
        "UTC".to_owned()
    })
}

/// The location name as the 0x24 bytes of the guest.
pub(crate) fn location_name() -> [u8; NAME_LEN] {
    padded(name_text())
}

/// `CalendarTime` (8 bytes) and `CalendarAdditionalInfo` (0x18 bytes) of the POSIX time `posix`.
///
/// `CalendarTime` is `{u16 year, u8 month, u8 day, u8 hour, u8 minute, u8 second, u8 pad}`. `CalendarAdditionalInfo` is
/// `{u32 wday, u32 yday, char tzname[8], u32 dst, i32 offset}`.
pub(crate) fn calendar(posix: i64) -> Option<[u8; CALENDAR_LEN]> {
    let tm = sys::local_time(posix)?;
    let mut b = [0u8; CALENDAR_LEN];
    b[0..2].copy_from_slice(&u16::try_from(tm.year).ok()?.to_le_bytes());
    b[2] = u8::try_from(tm.month).ok()?;
    b[3] = u8::try_from(tm.day).ok()?;
    b[4] = u8::try_from(tm.hour).ok()?;
    b[5] = u8::try_from(tm.minute).ok()?;
    b[6] = u8::try_from(tm.second).ok()?;
    b[8..12].copy_from_slice(&tm.wday.to_le_bytes());
    b[12..16].copy_from_slice(&tm.yday.to_le_bytes());
    b[16..24].copy_from_slice(&tm.zone);
    b[24..28].copy_from_slice(&u32::from(tm.dst).to_le_bytes());
    b[28..32].copy_from_slice(&tm.offset.to_le_bytes());
    Some(b)
}

/// The POSIX time of a `CalendarTime` in the local time of the host.
pub(crate) fn posix(calendar_time: &[u8]) -> Option<i64> {
    let b = calendar_time.get(..7)?;
    sys::to_posix(&Tm {
        year: i32::from(u16::from_le_bytes([b[0], b[1]])),
        month: u32::from(b[2]),
        day: u32::from(b[3]),
        hour: u32::from(b[4]),
        minute: u32::from(b[5]),
        second: u32::from(b[6]),
        ..Tm::default()
    })
}

/// `nn::timesrv::detail::service::ITimeZoneService`.
pub(crate) struct TimeZone;

impl TimeZone {
    fn to_calendar(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        let Some(t) = req.u64_at(0) else {
            return fail(rsp, RES_FAIL);
        };
        match calendar(t as i64) {
            Some(c) => {
                rsp.push_bytes(&c);
                Handled::Done
            }
            None => fail(rsp, RES_FAIL),
        }
    }

    fn to_posix(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        let Some(t) = req.data.get(..8).and_then(posix) else {
            return fail(rsp, RES_FAIL);
        };
        if out_buf(req, 0).is_none() {
            return fail(rsp, RES_FAIL);
        }
        let _ = write_out(req, 0, &t.to_le_bytes());
        rsp.push_u32(1);
        Handled::Done
    }
}

impl Service for TimeZone {
    fn name(&self) -> &'static str {
        "time::ITimeZoneService"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            0 => {
                rsp.push_bytes(&location_name());
                Handled::Done
            }
            2 => {
                rsp.push_u32(1);
                Handled::Done
            }
            3 => {
                if out_buf(req, 0).is_some() {
                    let _ = write_out(req, 0, &location_name());
                }
                rsp.push_u32(1);
                Handled::Done
            }
            4 => Handled::Done,
            100 | 101 => self.to_calendar(req, rsp),
            201 | 202 => self.to_posix(req, rsp),
            _ => Handled::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_location_name_has_the_size_of_the_guest_structure() {
        let n = location_name();
        assert_eq!(n.len(), NAME_LEN);
        assert_eq!(n[NAME_LEN - 1], 0);
        assert_ne!(n[0], 0);
    }

    #[test]
    fn a_calendar_time_converts_back() {
        let t = 1_700_000_000;
        let c = calendar(t);
        assert_eq!(c.as_ref().and_then(|c| posix(&c[..8])), Some(t));
    }

    #[test]
    fn a_short_calendar_time_is_refused() {
        assert_eq!(posix(&[0; 4]), None);
    }
}
