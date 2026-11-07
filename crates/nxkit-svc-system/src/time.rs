//! The `time:u`, `time:a` and `time:s` services: `IStaticService`, `ISystemClock` and `ISteadyClock`.
//!
//! Command IDs are from switchbrew `Time services` and libnx `time.c`. The values follow nxnative
//! (`dm/host/hle_time.c`):
//! - The user clock is the clock of the host. `Config::fixed_time` pins its start.
//! - The steady clock is the monotonic clock of the host in seconds, with the source ID [`STEADY_ID`].
//! - The network clock is offline and never accurate. Its context is zero and `GetCurrentTime` fails.
//! - The user clock context is `{s64 offset, steady time point}`. It is renewed only when the host clock drifts more
//!   than one second from it.
//!
//! The root commands are 0 `GetStandardUserSystemClock`, 1 `GetStandardNetworkSystemClock`, 2 `GetStandardSteadyClock`,
//! 3 `GetTimeZoneService`, 4 `GetStandardLocalSystemClock`, 20 `GetSharedMemoryNativeHandle`, 100
//! `IsStandardUserSystemClockAutomaticCorrectionEnabled`, 200 `IsStandardNetworkSystemClockAccuracySufficient` and 400
//! `GetClockSnapshot`. A clock answers 0 `GetCurrentTime` and 2 `GetSystemClockContext`. The steady clock answers 0
//! `GetCurrentTimePoint` and 200 `GetInternalOffset`. Setting a clock is unknown.
//!
//! The shared memory (0x1000 bytes) holds the steady clock at 0x00, the user context at 0x38 and the network context at
//! 0x80. Each is a counter of 4 bytes and two slots from offset 8, the slot is the counter modulo 2. The steady base
//! time is 0, so the guest computes the steady time from its system tick and this requires that the tick of the kernel
//! counts at the pace of `CLOCK_MONOTONIC` in nanoseconds.

use crate::config;
use crate::sys;
use crate::tz::{self, TimeZone};
use crate::util::{RES_FAIL, fail, lock, reply_buf};
use nxkit_abi::guest;
use nxkit_abi::ipc::{Handled, Request, Response, Service};
use nxkit_abi::kernel::kernel;
use std::sync::{Arc, Mutex, OnceLock};

/// The source ID of the steady clock. It names the clock of the nxkit host, so it differs from the ID of nxnative.
pub(crate) const STEADY_ID: [u8; 16] = *b"nxkit-steady-001";

const MEM_SIZE: u64 = 0x1000;
const STEADY_OFFSET: u64 = 0x00;
const USER_OFFSET: u64 = 0x38;
const NETWORK_OFFSET: u64 = 0x80;
pub(crate) const SNAPSHOT_LEN: usize = 0xD0;

/// The steady clock in seconds.
pub(crate) fn steady_secs() -> i64 {
    i64::try_from(sys::monotonic_ns() / 1_000_000_000).unwrap_or(0)
}

/// The time of the host clock in seconds since the epoch.
pub(crate) fn real_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

/// The time of the user clock in seconds since the epoch.
pub(crate) fn user_now() -> i64 {
    static START: OnceLock<i64> = OnceLock::new();
    match config().fixed_time {
        Some(fixed) => fixed + steady_secs() - *START.get_or_init(steady_secs),
        None => real_now(),
    }
}

/// A steady time point, `{s64 seconds, uuid}`.
pub(crate) fn steady_point() -> [u8; 0x18] {
    let mut b = [0u8; 0x18];
    b[..8].copy_from_slice(&steady_secs().to_le_bytes());
    b[8..].copy_from_slice(&STEADY_ID);
    b
}

/// The user clock context, `{s64 offset, steady time point}`.
pub(crate) fn user_context() -> [u8; 0x20] {
    let mut b = [0u8; 0x20];
    b[..8].copy_from_slice(&(user_now() - steady_secs()).to_le_bytes());
    b[8..].copy_from_slice(&steady_point());
    b
}

/// The network clock context. The network clock is never set.
pub(crate) fn network_context() -> [u8; 0x20] {
    [0u8; 0x20]
}

struct Shared {
    handle: u32,
    addr: u64,
    offset: i64,
    counters: [u32; 3],
}

static SHARED: Mutex<Option<Shared>> = Mutex::new(None);

impl Shared {
    /// Stores `data` in the slot that the guest does not read and then publishes it with the counter.
    fn put(&mut self, object: usize, at: u64, data: &[u8]) {
        let c = self.counters[object].wrapping_add(1);
        let slot = u64::from(c & 1);
        let base = self.addr + at;
        let _ = guest::write(base + 8 + slot * data.len() as u64, data);
        let _ = guest::write(base, &c.to_le_bytes());
        self.counters[object] = c;
    }

    fn put_user(&mut self) {
        self.offset = user_now() - steady_secs();
        self.put(1, USER_OFFSET, &user_context());
    }
}

/// The handle of the shared memory. The memory is made on the first call, and the user context is renewed when the host
/// clock drifted more than one second from it.
fn shared_handle() -> Option<u32> {
    let mut g = lock(&SHARED);
    if g.is_none() {
        let (handle, addr) = kernel()?.create_shared_memory(MEM_SIZE).ok()?;
        let mut s = Shared {
            handle,
            addr,
            offset: 0,
            counters: [0; 3],
        };
        let mut steady = [0u8; 0x18];
        steady[8..].copy_from_slice(&STEADY_ID);
        s.put(0, STEADY_OFFSET, &steady);
        s.put_user();
        s.put(2, NETWORK_OFFSET, &network_context());
        *g = Some(s);
    }
    let s = g.as_mut()?;
    if config().fixed_time.is_none() && (user_now() - steady_secs() - s.offset).abs() > 1 {
        s.put_user();
    }
    Some(s.handle)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    User,
    Network,
    Local,
}

/// `nn::timesrv::detail::service::ISystemClock`.
struct SystemClock(Kind);

impl Service for SystemClock {
    fn name(&self) -> &'static str {
        "time::ISystemClock"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match (req.command, self.0) {
            (0, Kind::Network) => fail(rsp, RES_FAIL),
            (0, _) => {
                rsp.push_u64(user_now() as u64);
                Handled::Done
            }
            (2, Kind::Network) => {
                rsp.push_bytes(&network_context());
                Handled::Done
            }
            (2, _) => {
                rsp.push_bytes(&user_context());
                Handled::Done
            }
            _ => Handled::Unknown,
        }
    }
}

/// `nn::timesrv::detail::service::ISteadyClock`.
struct SteadyClock;

impl Service for SteadyClock {
    fn name(&self) -> &'static str {
        "time::ISteadyClock"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            0 => {
                rsp.push_bytes(&steady_point());
                Handled::Done
            }
            200 => {
                rsp.push_u64(0);
                Handled::Done
            }
            _ => Handled::Unknown,
        }
    }
}

/// The 0xD0 bytes of `nn::time::sf::ClockSnapshot` for the user clock of the host and an offline network clock.
pub(crate) fn snapshot(clock_type: u8) -> Option<[u8; SNAPSHOT_LEN]> {
    let now = user_now();
    let cal = tz::calendar(now)?;
    let mut b = [0u8; SNAPSHOT_LEN];
    b[0x00..0x20].copy_from_slice(&user_context());
    b[0x20..0x40].copy_from_slice(&network_context());
    b[0x40..0x48].copy_from_slice(&now.to_le_bytes());
    b[0x50..0x58].copy_from_slice(&cal[..8]);
    b[0x60..0x78].copy_from_slice(&cal[8..]);
    b[0x90..0xA8].copy_from_slice(&steady_point());
    b[0xA8..0xCC].copy_from_slice(&tz::location_name());
    b[0xCD] = clock_type;
    Some(b)
}

/// The root of `time:u`, `time:a` and `time:s`, `nn::timesrv::detail::service::IStaticService`.
pub(crate) struct Time {
    pub(crate) name: &'static str,
}

impl Service for Time {
    fn name(&self) -> &'static str {
        self.name
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        let object: Arc<dyn Service> = match req.command {
            0 => Arc::new(SystemClock(Kind::User)),
            1 => Arc::new(SystemClock(Kind::Network)),
            2 => Arc::new(SteadyClock),
            3 => Arc::new(TimeZone),
            4 => Arc::new(SystemClock(Kind::Local)),
            20 => {
                return match shared_handle() {
                    Some(h) => {
                        rsp.copy_handles.push(h);
                        Handled::Done
                    }
                    None => fail(rsp, RES_FAIL),
                };
            }
            100 | 200 => {
                rsp.push_bytes(&[0]);
                return Handled::Done;
            }
            400 => {
                let clock_type = req.data.first().copied().unwrap_or(0);
                return match snapshot(clock_type) {
                    Some(b) => reply_buf(req, 0, &b, rsp),
                    None => fail(rsp, RES_FAIL),
                };
            }
            _ => return Handled::Unknown,
        };
        rsp.objects.push(object);
        Handled::Done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_contexts_have_the_size_of_the_guest_structure() {
        assert_eq!(user_context().len(), 0x20);
        assert_eq!(network_context(), [0u8; 0x20]);
    }

    #[test]
    fn the_context_offset_makes_the_user_time() {
        let c = user_context();
        let offset = i64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
        let steady = i64::from_le_bytes([c[8], c[9], c[10], c[11], c[12], c[13], c[14], c[15]]);
        assert!((offset + steady - user_now()).abs() <= 1);
    }

    #[test]
    fn the_snapshot_holds_the_clock_type() {
        let s = snapshot(2);
        assert_eq!(s.map(|b| b[0xCD]), Some(2));
    }
}
