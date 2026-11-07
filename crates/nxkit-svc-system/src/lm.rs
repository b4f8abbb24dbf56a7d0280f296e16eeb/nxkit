//! The telemetry and logging services `prepo:u` and `lm`.
//!
//! `prepo:u` (`nn::prepo::detail::ipc::IPrepoService`): nxnative saves a play report and sends nothing
//! (`dm/host/hle_offline.c`, `PlayReport::Save`
//! and `RequestImmediateTransmission`). IDs are from switchbrew `Play Report services`. The save
//! commands 10100 to 10107 and 10500 and 10501 (`SendReport`), the immediate transmission 10200 and the system
//! session ID 10400 are answered. 10300 `GetTransmissionStatus` gives 0. The report data is dropped.
//!
//! `lm` (`nn::lm::ILogService`): 0 `OpenLogger` returns `ILogger`. 0 `Log` writes the text of the log packet to the
//! error output of the host. 1 `SetDestination` is accepted. IDs are from switchbrew `Log services`. nxnative has no
//! counterpart.

use crate::util::read_in;
use nxkit_abi::ipc::{Handled, Request, Response, Service};
use std::sync::Arc;

/// The most bytes of one log packet that the host prints.
const LOG_LIMIT: usize = 0x1000;

/// `prepo:u`.
pub(crate) struct Prepo;

impl Service for Prepo {
    fn name(&self) -> &'static str {
        "prepo:u"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            10100..=10107 | 10200 | 10500 | 10501 => {}
            10300 => rsp.push_u32(0),
            10400 => rsp.push_u64(1),
            _ => return Handled::Unknown,
        }
        Handled::Done
    }
}

/// `lm`.
pub(crate) struct Lm;

impl Service for Lm {
    fn name(&self) -> &'static str {
        "lm"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            0 => {
                rsp.objects.push(Arc::new(Logger));
                Handled::Done
            }
            _ => Handled::Unknown,
        }
    }
}

/// `nn::lm::ILogger`.
struct Logger;

/// The printable text of a log packet. Control bytes become spaces and the packet header shows as odd characters.
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

impl Service for Logger {
    fn name(&self) -> &'static str {
        "lm::ILogger"
    }

    fn handle(&self, req: &Request<'_>, _rsp: &mut Response) -> Handled {
        match req.command {
            0 => {
                if let Some(bytes) = read_in(req, 0, LOG_LIMIT) {
                    eprintln!("nxkit-svc-system: lm: {}", text(&bytes).trim());
                }
                Handled::Done
            }
            1 => Handled::Done,
            _ => Handled::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_bytes_become_spaces() {
        assert_eq!(text(b"a\nb\0c"), "a b c");
    }
}
