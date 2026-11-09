//! The service manager `sm:`.
//!
//! `Initialize` succeeds. `GetService` returns a session for a registered service. `RegisterService` and
//! `UnregisterService` are refused, because the game serves no port of its own. A name that no
//! service crate registered stops the game with the name (R15).

use crate::{Registry, Stop, StopSlot};
use nxkit_abi::ipc::{Handled, Request, Response, Service};
use std::sync::Arc;

/// The result `NotAllowed` of the module `sm` (21): description 8. Unverified: taken from the public Horizon result
/// list, not from the engine, which does not serve `sm:` for fs.
const RESULT_NOT_ALLOWED: u32 = 21 | 8 << 9;

pub(crate) const INITIALIZE: u32 = 0;
pub(crate) const GET_SERVICE: u32 = 1;
pub(crate) const REGISTER_SERVICE: u32 = 2;
pub(crate) const UNREGISTER_SERVICE: u32 = 3;

/// The root interface of the port `sm:`.
pub(crate) struct Sm {
    pub(crate) registry: Arc<Registry>,
    pub(crate) stop: Arc<StopSlot>,
}

/// The service name in the first 8 bytes of the data: ASCII, padded with NUL.
pub(crate) fn service_name(data: &[u8]) -> String {
    let raw = &data[..data.len().min(8)];
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..end]).into_owned()
}

impl Service for Sm {
    fn name(&self) -> &'static str {
        "sm:"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            INITIALIZE => Handled::Done,
            GET_SERVICE => {
                let name = service_name(req.data);
                match self.registry.get(name.as_str()) {
                    Some(root) => {
                        rsp.objects.push(root.clone());
                        Handled::Done
                    }
                    None => {
                        self.stop.record(Stop {
                            service: name,
                            interface: "sm:".to_string(),
                            command: GET_SERVICE,
                            reason: "no service crate registers this name".to_string(),
                        });
                        Handled::Unknown
                    }
                }
            }
            REGISTER_SERVICE | UNREGISTER_SERVICE => {
                rsp.result = RESULT_NOT_ALLOWED;
                Handled::Done
            }
            _ => Handled::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_name_stops_at_the_first_nul() {
        assert_eq!(service_name(b"fsp-srv\0"), "fsp-srv");
        assert_eq!(service_name(b"hid\0\0\0\0\0rest"), "hid");
        assert_eq!(service_name(b"audren:u"), "audren:u");
        assert_eq!(service_name(b""), "");
    }
}
