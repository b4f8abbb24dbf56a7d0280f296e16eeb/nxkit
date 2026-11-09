//! The `apm` and `apm:p` services (`IManager` and `ISession`) and the `psm` service.
//!
//! IDs and layouts are from libnx `apm.c` and `psm.c` and from switchbrew `APM services` and `PTM services`.
//!
//! `apm`, `apm:p`: 0 `OpenSession`, 1 `GetPerformanceMode`, 6 `IsCpuOverclockEnabled`.
//!
//! `ISession`: 0 `SetPerformanceConfiguration` (logged and ignored, as `SetPerformanceConfiguration` of the engine
//! `dm/host/hle_oe.c`), 1 `GetPerformanceConfiguration` (`0x00010001` for the boost mode and `0x00010000` for the normal
//! mode) and 2 `SetCpuOverclockEnabled`.
//!
//! `psm` has no counterpart in the engine, which has no battery. The host reports a docked console with a full battery:
//! 0 `GetBatteryChargePercentage` gives 100, 1 `GetChargerType` gives `EnoughPower`, 2 to 6 and 8 to 11 are accepted,
//! 4 `IsBatteryChargingEnabled` gives false, 12 `GetBatteryVoltageState` gives `Normal`, 13
//! `GetRawBatteryChargePercentage` gives 100.0, 14 `IsEnoughPowerSupplied` gives true, 15 `GetBatteryAgePercentage`
//! gives 100.0 and 7 `OpenSession` returns the session. The session answers 0 `BindStateChangeEvent` with an event that
//! is never signalled, 1 `UnbindStateChangeEvent` and 2 to 4 the `Set...ChangeEventEnabled` commands.

use crate::state::state;
use crate::util::Event;
use nxkit_abi::ipc::{Handled, Request, Response, Service};
use std::sync::Arc;

/// The value of `GetPerformanceConfiguration` for a performance mode.
fn configuration(mode: u32) -> u32 {
    if mode == 1 { 0x0001_0001 } else { 0x0001_0000 }
}

/// `apm` and `apm:p`.
pub(crate) struct Apm {
    pub(crate) name: &'static str,
}

impl Service for Apm {
    fn name(&self) -> &'static str {
        self.name
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            0 => rsp.objects.push(Arc::new(Session)),
            1 => rsp.push_u32(state().performance_mode()),
            6 => rsp.push_bytes(&[u8::from(state().with(|i| i.cpu_overclock))]),
            _ => return Handled::Unknown,
        }
        Handled::Done
    }
}

/// `nn::apm::ISession`.
struct Session;

impl Service for Session {
    fn name(&self) -> &'static str {
        "apm::ISession"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            0 => eprintln!(
                "nxkit-svc-system: SetPerformanceConfiguration mode {} configuration {:#x}",
                req.u32_at(0).unwrap_or(0),
                req.u32_at(4).unwrap_or(0)
            ),
            1 => rsp.push_u32(configuration(req.u32_at(0).unwrap_or(0))),
            2 => {
                let on = req.data.first().is_some_and(|b| *b != 0);
                state().with(|i| i.cpu_overclock = on);
            }
            _ => return Handled::Unknown,
        }
        Handled::Done
    }
}

/// `nn::psm::IPsmServer`.
pub(crate) struct Psm;

impl Service for Psm {
    fn name(&self) -> &'static str {
        "psm"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            0 => rsp.push_u32(100),
            // PsmChargerType_EnoughPower
            1 => rsp.push_u32(1),
            2..=3 | 5..=6 | 8..=11 => {}
            4 | 14 => rsp.push_bytes(&[u8::from(req.command == 14)]),
            7 => rsp.objects.push(Arc::new(PsmSession {
                event: Event::new(),
            })),
            // PsmBatteryVoltageState_Normal
            12 => rsp.push_u32(3),
            13 | 15 => rsp.push_u64(100.0f64.to_bits()),
            _ => return Handled::Unknown,
        }
        Handled::Done
    }
}

/// `nn::psm::IPsmSession`.
struct PsmSession {
    event: Event,
}

impl Service for PsmSession {
    fn name(&self) -> &'static str {
        "psm::IPsmSession"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            0 => self.event.reply(rsp, false),
            1..=4 => Handled::Done,
            _ => Handled::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_configuration_follows_the_mode() {
        assert_eq!(configuration(1), 0x0001_0001);
        assert_eq!(configuration(0), 0x0001_0000);
    }
}
