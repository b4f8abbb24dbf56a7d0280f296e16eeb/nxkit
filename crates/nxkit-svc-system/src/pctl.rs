//! The parental control services `pctl`, `pctl:a`, `pctl:s` and `pctl:r`.
//!
//! IDs and layouts are from libnx `pctl.c` and switchbrew `Parental Control services`. The values follow nxnative,
//! which has one player and no parental controls: no restriction is enabled, the safety level is 0, free communication
//! is never allowed and stereo vision is not permitted (`dm/host/hle_misc.c`, the `nn::pctl` functions
//! `IsFreeCommunicationAvailable`, `GetStereoVisionRestriction` and `CheckAgcPermissionUnderMinimumAge`).
//!
//! Root: 0 `CreateService` and 1 `CreateServiceWithoutInitialize` return `IParentalControlService`. Both take a
//! reserved u64 and the process ID.
//!
//! `IParentalControlService`: 1 `Initialize`, 1006 `IsRestrictionTemporaryUnlocked` (false), 1013
//! `ConfirmStereoVisionPermission` (accepted), 1031 `IsRestrictionEnabled` (false), 1032 `GetSafetyLevel` (0), 1035
//! `GetCurrentSettings` (rating age 0, no restrictions), 1039 `GetFreeCommunicationApplicationListCount` (0), 1064
//! `ResetConfirmedStereoVisionPermission` (accepted), 1065 `IsStereoVisionPermitted` (false), 1403 `IsPairingActive`
//! (false), 1432, 1457 and 1473 the event getters (never signalled) and 1458 `IsPlayTimerAlarmDisabled` (true).
//!
//! The permitted flag of 1065 is false because nxnative never confirms stereo vision. A value of true would make a game
//! that asks first behave differently from nxnative.

use crate::util::Event;
use nxkit_abi::ipc::{Handled, Request, Response, Service};
use std::sync::Arc;

/// The four `pctl` root services.
pub(crate) struct Pctl {
    pub(crate) name: &'static str,
}

impl Service for Pctl {
    fn name(&self) -> &'static str {
        self.name
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            0 | 1 => {
                rsp.objects.push(Arc::new(ParentalControl {
                    synchronization: Event::new(),
                    suspension: Event::new(),
                    unlinked: Event::new(),
                }));
                Handled::Done
            }
            _ => Handled::Unknown,
        }
    }
}

/// `nn::pctl::detail::ipc::IParentalControlService`.
struct ParentalControl {
    synchronization: Event,
    suspension: Event,
    unlinked: Event,
}

impl Service for ParentalControl {
    fn name(&self) -> &'static str {
        "pctl::IParentalControlService"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            1 | 1013 | 1064 => {}
            1006 | 1031 | 1065 | 1403 => rsp.push_bytes(&[0]),
            1458 => rsp.push_bytes(&[1]),
            1032 | 1039 => rsp.push_u32(0),
            // PctlRestrictionSettings: rating age, SNS post restriction, free communication restriction
            1035 => rsp.push_bytes(&[0, 0, 0]),
            1432 => return self.synchronization.reply(rsp, false),
            1457 => return self.suspension.reply(rsp, false),
            1473 => return self.unlinked.reply(rsp, false),
            _ => return Handled::Unknown,
        }
        Handled::Done
    }
}
