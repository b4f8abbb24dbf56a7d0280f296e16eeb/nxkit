//! The content services `bcat:u` and `aoc:u`.
//!
//! `bcat:u` (`nn::bcat::ipc::IServiceCreator`): The engine initialises BCAT as a no-op and fails the delivery cache
//! mount (`dm/host/hle_offline.c`, `nn::bcat::Initialize` and
//! `MountDeliveryCacheStorage`). IDs are from switchbrew `BCAT services`. The root
//! commands 0 `CreateBcatService` returns `IBcatService` and 1 and 2 return `IDeliveryCacheStorageService`.
//! `IBcatService` accepts 10200 `CancelSyncDeliveryCacheRequest` and 30100 `SetPassphrase`. The sync and task commands
//! (10100, 10101, 20100, 20101, 20300, 20301, 20400, 20401, 20410 and 20500) fail with `RES_FAIL`.
//! `IDeliveryCacheStorageService` fails 0, 1 and 10, so the game sees an empty delivery cache that cannot be mounted.
//!
//! `aoc:u` (`nn::aoc::IAddOnContentManager`): The engine counts add-on content in a directory of the host, and this
//! service has none (`dm/host/hle_fs.c`, `MountAddOnContent`). IDs are from switchbrew `NS` (the `aoc:u` table).
//! 2 `CountAddOnContent` and 3 `ListAddOnContent` give 0, 5 `GetAddOnContentBaseId` gives 0, 7 `PrepareAddOnContent` is
//! accepted, 8 and 10 (the list changed event) give an event that is never signalled and 9
//! `GetAddOnContentLostErrorCode` gives 0.

use crate::util::{Event, RES_FAIL, fail};
use nxkit_abi::ipc::{Handled, Request, Response, Service};
use std::sync::Arc;

/// `bcat:u`.
pub(crate) struct Bcat;

impl Service for Bcat {
    fn name(&self) -> &'static str {
        "bcat:u"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            0 => rsp.objects.push(Arc::new(BcatService)),
            1 | 2 => rsp.objects.push(Arc::new(DeliveryCacheStorage)),
            _ => return Handled::Unknown,
        }
        Handled::Done
    }
}

/// `nn::bcat::ipc::IBcatService`.
struct BcatService;

impl Service for BcatService {
    fn name(&self) -> &'static str {
        "bcat::IBcatService"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            10200 | 30100 => Handled::Done,
            10100 | 10101 | 20100 | 20101 | 20300 | 20301 | 20400 | 20401 | 20410 | 20500 => {
                fail(rsp, RES_FAIL)
            }
            _ => Handled::Unknown,
        }
    }
}

/// `nn::bcat::ipc::IDeliveryCacheStorageService`.
struct DeliveryCacheStorage;

impl Service for DeliveryCacheStorage {
    fn name(&self) -> &'static str {
        "bcat::IDeliveryCacheStorageService"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            0 | 1 | 10 => fail(rsp, RES_FAIL),
            _ => Handled::Unknown,
        }
    }
}

/// `aoc:u`.
pub(crate) struct Aoc {
    changed: Event,
}

impl Aoc {
    /// A service whose list never changes.
    pub(crate) const fn new() -> Aoc {
        Aoc {
            changed: Event::new(),
        }
    }
}

impl Service for Aoc {
    fn name(&self) -> &'static str {
        "aoc:u"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            2 | 3 | 9 => rsp.push_u32(0),
            5 => rsp.push_u64(0),
            7 => {}
            8 | 10 => return self.changed.reply(rsp, false),
            _ => return Handled::Unknown,
        }
        Handled::Done
    }
}
