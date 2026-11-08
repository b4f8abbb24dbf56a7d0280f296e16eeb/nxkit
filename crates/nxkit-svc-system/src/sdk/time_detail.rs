//! The `nn::time` functions of the time service that the SDK asks for through its session. Source: nxnative
//! `dm/host/hle_time.c` (the periodic benefit claim and the two `detail::` questions).
//!
//! The claim of a periodic benefit (Tetris 99 2.5.0's daily reward) aborts in the SDK unless the time service session
//! is set, and the host's `Initialize` opens none. Past that check the public function only calls the `detail::`
//! implementation of the game with the same arguments, so the export does that. The implementation compares the
//! snapshots and asks the service two questions, which the exports below answer. The host knows neither moment, so the
//! time point comes back cleared. Its source ID matches no snapshot, and the claim then uses only the snapshots.

use super::{fatal, log, put_bytes, put_zero};
use crate::seams::guest_sym;
use crate::sys::call_guest;
use crate::tz;
use std::sync::atomic::{AtomicU64, Ordering};

const DETAIL_CLAIM: &str = "_ZN2nn4time6detail39ClaimPeriodicBenefitWithUserSystemClockEPNS0_27PeriodicBenefitClaimContextERKNS0_13ClockSnapshotERKNS_8TimeSpanES9_PKNS0_25CheckCalendarTimeCallbackE";

static DETAIL_CLAIM_ADDR: AtomicU64 = AtomicU64::new(0);

/// The size of a `SteadyClockTimePoint`: `{s64 seconds, uuid}`.
const TIME_POINT_LEN: u64 = 0x18;

/// `ClaimPeriodicBenefitWithUserSystemClock(PeriodicBenefitClaimContext*, const ClockSnapshot&, const TimeSpan&,
/// const TimeSpan&, const CheckCalendarTimeCallback*)`: periodic benefit claim (time service session check); the
/// SDK's detail implementation compares the snapshots.
extern "C" fn claim_periodic_benefit(ctx: u64, snapshot: u64, max_penalty: u64, acceptable: u64, callback: u64) -> u64 {
    let mut addr = DETAIL_CLAIM_ADDR.load(Ordering::Relaxed);
    if addr == 0 {
        addr = guest_sym(DETAIL_CLAIM).unwrap_or_else(|| {
            fatal("time: the sdk has ClaimPeriodicBenefitWithUserSystemClock but not its detail:: implementation")
        });
        DETAIL_CLAIM_ADDR.store(addr, Ordering::Relaxed);
    }
    let r = call_guest(addr, [ctx, snapshot, max_penalty, acceptable, callback, 0]);
    log(&format!("time: periodic benefit claim → {}", r & 0xffff_ffff));
    r
}

/// `detail::GetStandardUserSystemClockAutomaticCorrectionUpdatedTime(SteadyClockTimePoint*)`: when automatic
/// correction was last switched (time service); not known on the host: a time point of no clock.
extern "C" fn automatic_correction_updated_time(point: u64) -> u32 {
    put_zero(point, TIME_POINT_LEN);
    0
}

/// `detail::GetDeviceLocationNameAndUpdatedTime(LocationName*, SteadyClockTimePoint*)`: device location and when it
/// was set (time service): the zone of the host, as in its snapshots, and a time point of no clock.
extern "C" fn device_location_name_and_updated_time(name: u64, point: u64) -> u32 {
    put_bytes(name, &tz::location_name());
    put_zero(point, TIME_POINT_LEN);
    0
}

table! {
    "_ZN2nn4time39ClaimPeriodicBenefitWithUserSystemClockEPNS0_27PeriodicBenefitClaimContextERKNS0_13ClockSnapshotERKNS_8TimeSpanES8_PKNS0_25CheckCalendarTimeCallbackE" => claim_periodic_benefit,
    "_ZN2nn4time6detail56GetStandardUserSystemClockAutomaticCorrectionUpdatedTimeEPNS0_20SteadyClockTimePointE" => automatic_correction_updated_time,
    "_ZN2nn4time6detail35GetDeviceLocationNameAndUpdatedTimeEPNS0_12LocationNameEPNS0_20SteadyClockTimePointE" => device_location_name_and_updated_time,
}
