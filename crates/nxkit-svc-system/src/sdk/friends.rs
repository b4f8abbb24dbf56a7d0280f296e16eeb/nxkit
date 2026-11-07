//! `nn::friends` take-overs. Source: nxnative `dm/host/hle_friends.c`.
//!
//! The friends service is not available. Every call that needs it fails, a `bool` call gives false, and the events are
//! bound and never fire. The getters of `Friend`, `Profile` and `FriendPresence` and the constructors of the value
//! types run in the SDK, because they read and write the objects of the game only.

use super::{fail, nothing, put_u32, zero};
use crate::event::{self, Block};

static NOTIFY_EVENT: Block = Block::new();
static INVITE_EVENT: Block = Block::new();

const EVENT_BYTES: u64 = 0x100;

/// An event that never fires, bound on first use.
fn quiet_event(block: &Block) -> u64 {
    let ev = block.addr(EVENT_BYTES);
    event::bind(ev, false);
    ev
}

/// `AsyncContext::GetSystemEvent`: the completion event fires at once.
extern "C" fn completion_event(_this: u64, ev: u64) -> u32 {
    event::signal(ev);
    0
}

/// `GetFriendList(int*, Friend*, int, int, const FriendFilter&)`: empty, and the call fails.
extern "C" fn friend_list(count: u64) -> u32 {
    put_u32(count, 0);
    super::RES_FAIL
}

/// `GetBlockedUserList` without a `Uid` (SDK 4): empty and successful.
extern "C" fn blocked_without_uid(count: u64) -> u32 {
    put_u32(count, 0);
    0
}

/// `GetBlockedUserList` with a `Uid`: empty, and the call fails.
extern "C" fn blocked_with_uid(count: u64) -> u32 {
    put_u32(count, 0);
    super::RES_FAIL
}

/// `NotificationQueue::GetSystemEvent`.
extern "C" fn notification_event(_this: u64) -> u64 {
    quiet_event(&NOTIFY_EVENT)
}

/// `GetFriendInvitationNotificationEvent`.
extern "C" fn invitation_event() -> u64 {
    quiet_event(&INVITE_EVENT)
}

table! {
    "_ZN2nn7friends10InitializeEv" => nothing,
    "_ZN2nn7friends12AsyncContextC1Ev" => nothing,
    "_ZN2nn7friends12AsyncContextD1Ev" => nothing,
    "_ZN2nn7friends12AsyncContext14GetSystemEventEPNS_2os11SystemEventE" => completion_event,
    "_ZNK2nn7friends12AsyncContext9GetResultEv" => fail,
    "_ZN2nn7friends13GetFriendListEPiPNS0_6FriendEiiRKNS0_12FriendFilterE" => friend_list,
    "_ZN2nn7friends18GetBlockedUserListEPiPNS_7account23NetworkServiceAccountIdEii" => blocked_without_uid,
    "_ZN2nn7friends14GetProfileListEPNS0_12AsyncContextEPNS0_7ProfileERKNS_7account3UidEPKNS5_23NetworkServiceAccountIdEi" => fail,
    "_ZN2nn7friends16UpdateFriendInfoEPNS0_6FriendERKNS_7account3UidEPKNS3_23NetworkServiceAccountIdEi" => fail,
    "_ZN2nn7friends14AddPlayHistoryERKNS0_26PlayHistoryRegistrationKeyERKNS0_15InAppScreenNameES6_" => fail,
    "_ZN2nn7friends29GetPlayHistoryRegistrationKeyEPNS0_26PlayHistoryRegistrationKeyERKNS_7account3UidEb" => fail,
    "_ZN2nn7friends17NotificationQueueC1Ev" => nothing,
    "_ZN2nn7friends17NotificationQueueD1Ev" => nothing,
    "_ZN2nn7friends17NotificationQueue10InitializeERKNS_7account3UidE" => fail,
    "_ZN2nn7friends17NotificationQueue14GetSystemEventEv" => notification_event,
    "_ZN2nn7friends17NotificationQueue3PopEPNS0_16NotificationInfoE" => fail,
    "_ZN2nn7friends36GetFriendInvitationNotificationEventEv" => invitation_event,
    "_ZN2nn7friends38TryPopFriendInvitationNotificationInfoEPNS_7account3UidEPmPvm" => zero, // no invitation
    "_ZN2nn7friends21StartFriendInvitationERKNS_7account3UidEiRKNS0_35FriendInvitationGameModeDescriptionEPKvm" => fail,
    "_ZN2nn7friends28StartSendingFriendInvitationERKNS_7account3UidEPKNS1_23NetworkServiceAccountIdEiRKNS0_35FriendInvitationGameModeDescriptionEPKvm" => fail,
    "_ZN2nn7friends18ShowUserDetailInfoERKNS_7account3UidERKNS1_23NetworkServiceAccountIdERKNS0_15InAppScreenNameESA_" => fail,
    "_ZN2nn7friends14ShowFriendListERKNS_7account3UidE" => fail,
    "_ZN2nn7friends19ShowBlockedUserListERKNS_7account3UidE" => fail,
    "_ZN2nn7friends29ShowReceivedFriendRequestListERKNS_7account3UidE" => fail,
    "_ZN2nn7friends23StartFacedFriendRequestERKNS_7account3UidE" => fail,
    "_ZN2nn7friends25StartSendingFriendRequestERKNS_7account3UidERKNS1_23NetworkServiceAccountIdERKNS0_15InAppScreenNameESA_" => fail,
    "_ZN2nn7friends33ShowMethodsOfSendingFriendRequestERKNS_7account3UidE" => fail,
    "_ZN2nn7friends18GetBlockedUserListEPiPNS_7account23NetworkServiceAccountIdERKNS2_3UidEii" => blocked_with_uid,
    "_ZN2nn7friends28DeclareOpenOnlinePlaySessionERKNS_7account3UidE" => fail,
    "_ZN2nn7friends29DeclareCloseOnlinePlaySessionERKNS_7account3UidE" => fail,
    "_ZN2nn7friends12UserPresence10InitializeERKNS_7account3UidE" => fail,
    "_ZN2nn7friends12UserPresence14SetDescriptionEPKc" => fail,
    "_ZN2nn7friends12UserPresence6CommitEv" => fail,
}
