//! The network interface service `nifm:u`. The host is offline, as nxnative (`dm/host/hle_net.c`, `SubmitNetworkRequest`
//! and `IsNetworkRequestOnHold`): no network is ever available, the first request query after `Submit` is on hold and every later query
//! is not available.
//!
//! IDs and layouts are from libnx `nifm.c` and switchbrew `NIFM services`.
//!
//! Root: 4 `CreateGeneralServiceOld` and 5 `CreateGeneralService` return `IGeneralService`.
//!
//! `IGeneralService`: 1 `GetClientId` (client 1), 4 `CreateRequest` (`IRequest`), 16 `SetWirelessCommunicationEnabled`
//! (accepted), 17 `IsWirelessCommunicationEnabled`, 20 `IsEthernetCommunicationEnabled`, 21
//! `IsAnyInternetRequestAccepted` and 22 `IsAnyForegroundRequestAccepted` (all false) and 18
//! `GetInternetConnectionStatus` (not connected). The commands for the network profile and the IP address (5, 7, 8, 9,
//! 12 and 15) fail, because there is no network.
//!
//! `IRequest`: 0 `GetRequestState`, 1 `GetResult` (fails), 2 `GetSystemEventReadableHandles` (two events, the state
//! event is signalled by `Submit`), 3 `Cancel`, 4 `Submit` and 5 `SetRequirement`.

use crate::util::{Event, RES_FAIL, fail, reply_buf};
use nxkit_abi::ipc::{Handled, Request, Response, Service};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

/// `NifmRequestState_Unknown1`, the state before `Submit` and after the first query.
const STATE_IDLE: u32 = 1;

/// `NifmRequestState_OnHold`.
const STATE_ON_HOLD: u32 = 2;

/// `nifm:u`.
pub(crate) struct Nifm;

impl Service for Nifm {
    fn name(&self) -> &'static str {
        "nifm:u"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            4 | 5 => {
                rsp.objects.push(Arc::new(General));
                Handled::Done
            }
            _ => Handled::Unknown,
        }
    }
}

/// `nn::nifm::detail::IGeneralService`.
struct General;

impl Service for General {
    fn name(&self) -> &'static str {
        "nifm::IGeneralService"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            1 => return reply_buf(req, 0, &1u32.to_le_bytes(), rsp),
            4 => rsp.objects.push(Arc::new(NetRequest {
                submitted: AtomicU8::new(0),
                state: Event::new(),
                other: Event::new(),
            })),
            5 | 7 | 8 | 9 | 12 | 15 => return fail(rsp, RES_FAIL),
            16 => {}
            17 | 20 | 21 | 22 => rsp.push_bytes(&[0]),
            // connection type, signal strength and status: none, none, ConnectingUnknown1
            18 => rsp.push_bytes(&[0, 0, 0]),
            _ => return Handled::Unknown,
        }
        Handled::Done
    }
}

/// `nn::nifm::detail::IRequest`.
struct NetRequest {
    /// 0 before `Submit`, 1 after `Submit` until the first state query, 2 afterwards.
    submitted: AtomicU8,
    state: Event,
    other: Event,
}

impl Service for NetRequest {
    fn name(&self) -> &'static str {
        "nifm::IRequest"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            0 => {
                let on_hold = self
                    .submitted
                    .compare_exchange(1, 2, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok();
                rsp.push_u32(if on_hold { STATE_ON_HOLD } else { STATE_IDLE });
            }
            1 => return fail(rsp, RES_FAIL),
            2 => {
                let (Some(a), Some(b)) = (self.state.readable(), self.other.readable()) else {
                    return fail(rsp, RES_FAIL);
                };
                rsp.copy_handles.push(a);
                rsp.copy_handles.push(b);
            }
            3 | 5 => {}
            4 => {
                self.submitted.store(1, Ordering::SeqCst);
                self.state.signal();
            }
            _ => return Handled::Unknown,
        }
        Handled::Done
    }
}
