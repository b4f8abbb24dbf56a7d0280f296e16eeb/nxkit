//! The friend service `friend:u`. The console is offline, so the friend list is empty and no notification arrives
//! (nxnative `dm/host/hle_offline.c`, `GetFriendList` and `CheckFriendListAvailability`, and `dm/host/hle_friends.c`).
//!
//! IDs are from switchbrew `Friend services` and libnx `friends.c`.
//!
//! Root: 0 `CreateFriendService` returns `IFriendService` and 1 `CreateNotificationService` (takes a user ID) returns
//! `INotificationService`.
//!
//! `IFriendService`: 0 `GetCompletionEvent` (never signalled), 1 `Cancel`, 10100 `GetFriendListIds`, 10101
//! `GetFriendList`, 10400 `GetBlockedUserListIds`, 10500 to 10503 `GetProfileList` (all with a count of 0), 20100
//! `GetFriendCount`, 20101 `GetNewlyFriendCount` and 20200 `GetReceivedFriendRequestCount` (0), 10600 `DeclareOpenOnlinePlaySession`,
//! 10601 `DeclareCloseOnlinePlaySession` and 10610 `UpdateUserPresence` (accepted and not sent). The commands that
//! need the network (10102, 10110, 10120, 10121, 10200, 10211, 10420, 10421, 10700 to 10702 and 20102 to 20104)
//! fail with `RES_FAIL`.
//!
//! `INotificationService`: 0 `GetEvent` (never signalled) and 1 `Clear`. 2 `Pop` fails, because the queue is empty.
//!
//! The in and out layouts of the list commands are not checked against a libnx source. The commands answer with a
//! count of 0, which is correct for any layout that has the count as its only output.

use crate::util::{Event, RES_FAIL, fail};
use nxkit_abi::ipc::{Handled, Request, Response, Service};
use std::sync::Arc;

/// `friend:u`.
pub(crate) struct Friend;

impl Service for Friend {
    fn name(&self) -> &'static str {
        "friend:u"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            0 => rsp.objects.push(Arc::new(FriendService {
                completion: Event::new(),
            })),
            1 => rsp.objects.push(Arc::new(Notification {
                event: Event::new(),
            })),
            _ => return Handled::Unknown,
        }
        Handled::Done
    }
}

/// `nn::friends::detail::ipc::IFriendService`.
struct FriendService {
    completion: Event,
}

impl Service for FriendService {
    fn name(&self) -> &'static str {
        "friend::IFriendService"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            0 => return self.completion.reply(rsp, false),
            1 | 10600 | 10601 | 10610 => {}
            10100 | 10101 | 10400 | 10500..=10503 | 20100 | 20101 | 20200 => rsp.push_u32(0),
            10102
            | 10110
            | 10120
            | 10121
            | 10200
            | 10211
            | 10420
            | 10421
            | 10700..=10702
            | 20102..=20104 => return fail(rsp, RES_FAIL),
            _ => return Handled::Unknown,
        }
        Handled::Done
    }
}

/// `nn::friends::detail::ipc::INotificationService`.
struct Notification {
    event: Event,
}

impl Service for Notification {
    fn name(&self) -> &'static str {
        "friend::INotificationService"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            0 => self.event.reply(rsp, false),
            1 => Handled::Done,
            2 => fail(rsp, RES_FAIL),
            _ => Handled::Unknown,
        }
    }
}
