//! `nn::account` take-overs.
//!
//! The host has one fixed local user. The online accounts fail, and the completion event of an asynchronous context
//! fires at once. The nickname comes from the configuration.

use super::oe_state::AppletGuard;
use super::{RES_FAIL, fail, fatal, get_u64, nothing, ok, put_bytes, put_u8, put_u32, put_u64};
use crate::acc::{UID, profile_jpeg};
use crate::config;
use crate::event;

/// `"USERHNDL"`, the tag in the third word of a `UserHandle`.
const HANDLE_MAGIC: u64 = 0x5553_4552_484e_444c;

fn put_uid(addr: u64) {
    put_u64(addr, UID[0]);
    put_u64(addr + 8, UID[1]);
}

fn put_handle(addr: u64, uid: [u64; 2]) {
    put_u64(addr, uid[0]);
    put_u64(addr + 8, uid[1]);
    put_u64(addr + 16, HANDLE_MAGIC);
}

/// `ListOpenUsers`, `ListAllUsers` and `ListQualifiedUsers`: the one user.
extern "C" fn list_users(count: u64, out: u64, max: u32) -> u32 {
    put_u32(count, 0);
    if max as i32 > 0 && out != 0 {
        put_uid(out);
        put_u32(count, 1);
    }
    0
}

/// `ShowUserSelector`: the host picks the local user.
extern "C" fn show_selector(out: u64, _settings: u64) -> u32 {
    let _applet = AppletGuard::new("user selector");
    put_uid(out);
    0
}

/// `PopOpenUsers`: the system opened none, so the game falls back to the preselected user.
extern "C" fn pop_open_users(count: u64) {
    put_u32(count, 0);
}

/// `TryOpenPreselectedUser`: gives a `bool`.
extern "C" fn try_open_preselected(handle: u64) -> u32 {
    put_handle(handle, UID);
    1
}

/// `OpenPreselectedUser`: SDK 6 returns a Result, SDK 7 and later ignore it.
extern "C" fn open_preselected(handle: u64) -> u32 {
    put_handle(handle, UID);
    0
}

/// `GetLastOpenedUser`.
extern "C" fn last_opened(out: u64) -> u32 {
    put_uid(out);
    0
}

/// `OpenUser`: the handle holds the uid itself.
extern "C" fn open_user(handle: u64, uid: u64) -> u32 {
    put_handle(handle, [get_u64(uid), get_u64(uid + 8)]);
    0
}

/// `GetUserId`: a handle that the host did not issue stops the game.
extern "C" fn user_id(out: u64, handle: u64) -> u32 {
    let magic = get_u64(handle + 16);
    if magic != HANDLE_MAGIC {
        fatal(&format!(
            "account::GetUserId: the handle was not issued by the host ({magic:#x})"
        ));
    }
    put_u64(out, get_u64(handle));
    put_u64(out + 8, get_u64(handle + 8));
    0
}

/// `GetNickname(Nickname*, const Uid&)`: 33 bytes of UTF-8.
extern "C" fn nickname(out: u64, _uid: u64) -> u32 {
    let mut b = [0u8; 33];
    let name = config().nickname.as_bytes();
    let n = name.len().min(32);
    b[..n].copy_from_slice(&name[..n]);
    put_bytes(out, &b);
    0
}

/// `GetUserCount`.
extern "C" fn user_count(out: u64) -> u32 {
    put_u32(out, 1);
    0
}

/// `GetUserExistence`: true for the local user.
extern "C" fn user_existence(out: u64, uid: u64) -> u32 {
    put_u8(
        out,
        u8::from(get_u64(uid) == UID[0] && get_u64(uid + 8) == UID[1]),
    );
    0
}

/// `ShowUserCreator`: the host already has its user.
extern "C" fn show_creator() -> u32 {
    let _applet = AppletGuard::new("user creator");
    0
}

/// `LoadProfileImage(uint64_t*, void*, size_t, const Uid&)`: the size is the image's, the copy what the buffer holds.
extern "C" fn load_profile_image(size: u64, buf: u64, cap: u64, _uid: u64) -> u32 {
    let image = profile_jpeg();
    if buf != 0 {
        let n = usize::try_from(cap).unwrap_or(usize::MAX).min(image.len());
        put_bytes(buf, &image[..n]);
    }
    put_u64(size, image.len() as u64);
    0
}

/// `IsNetworkServiceAccountAvailable`: never.
extern "C" fn network_account_available(out: u64, _handle: u64) -> u32 {
    put_u8(out, 0);
    0
}

/// A token or string of an online account: the size is 0 and the call fails. The size pointer is the first argument.
extern "C" fn token_first(size: u64) -> u32 {
    put_u64(size, 0);
    RES_FAIL
}

/// The same for a member function: the size pointer follows `this`.
extern "C" fn token_member(_this: u64, size: u64) -> u32 {
    put_u64(size, 0);
    RES_FAIL
}

/// `AsyncContext::GetSystemEvent` and the authorization context form: the event fires at once.
extern "C" fn completion_event(_this: u64, ev: u64) -> u32 {
    event::signal(ev);
    0
}

/// `AsyncContext::HasDone`: always done.
extern "C" fn has_done(_this: u64, done: u64) -> u32 {
    put_u8(done, 1);
    0
}

/// `AsyncContext::operator=(AsyncContext&&)`: the object holds no host state.
extern "C" fn move_assign(this: u64, _other: u64) -> u64 {
    this
}

table! {
    "_ZN2nn7account10InitializeEv" => nothing,
    "_ZN2nn7account13ListOpenUsersEPiPNS0_3UidEi" => list_users,
    "_ZN2nn7account12ListAllUsersEPiPNS0_3UidEi" => list_users,
    "_ZN2nn7account16ShowUserSelectorEPNS0_3UidERKNS0_21UserSelectionSettingsE" => show_selector,
    "_ZN2nn7account12PopOpenUsersEPiPNS0_10UserHandleEi" => pop_open_users,
    "_ZN2nn7account13PushOpenUsersEv" => nothing,
    "_ZN2nn7account22TryOpenPreselectedUserEPNS0_10UserHandleE" => try_open_preselected,
    "_ZN2nn7account19OpenPreselectedUserEPNS0_10UserHandleE" => open_preselected,
    "_ZN2nn7account17GetLastOpenedUserEPNS0_3UidE" => last_opened,
    "_ZN2nn7account8OpenUserEPNS0_10UserHandleERKNS0_3UidE" => open_user,
    "_ZN2nn7account9GetUserIdEPNS0_3UidERKNS0_10UserHandleE" => user_id,
    "_ZN2nn7account11GetNicknameEPNS0_8NicknameERKNS0_3UidE" => nickname,
    "_ZN2nn7account36EnsureNetworkServiceAccountAvailableERKNS0_10UserHandleE" => fail,
    "_ZN2nn7account26GetNetworkServiceAccountIdEPNS0_23NetworkServiceAccountIdERKNS0_10UserHandleE" => fail,
    "_ZN2nn7account37LoadNetworkServiceAccountIdTokenCacheEPmPcmRKNS0_10UserHandleE" => token_first,
    "_ZN2nn7account40ShowLicenseRequirementsForNetworkServiceERKNS0_10UserHandleE" => fail,
    "_ZN2nn7account29LoadCachedNintendoAccountInfoEPNS0_25CachedNintendoAccountInfoERKNS0_10UserHandleEPvm" => fail,
    "_ZN2nn7account12AsyncContextC1Ev" => nothing,
    "_ZN2nn7account44EnsureNetworkServiceAccountIdTokenCacheAsyncEPNS0_12AsyncContextERKNS0_10UserHandleE" => fail,
    "_ZN2nn7account12AsyncContext14GetSystemEventEPNS_2os11SystemEventE" => completion_event,
    "_ZN2nn7account12AsyncContext9GetResultEv" => fail,
    "_ZN2nn7account42NintendoAccountAuthorizationRequestContextC1Ev" => nothing,
    "_ZN2nn7account42NintendoAccountAuthorizationRequestContextD1Ev" => nothing,
    "_ZN2nn7account41CreateNintendoAccountAuthorizationRequestEPNS0_42NintendoAccountAuthorizationRequestContextERKNS0_10UserHandleERKNS0_45NintendoAccountAuthorizationRequestParametersEPvm" => fail,
    "_ZN2nn7account42NintendoAccountAuthorizationRequestContext14GetSystemEventEPNS_2os11SystemEventE" => completion_event,
    "_ZN2nn7account42NintendoAccountAuthorizationRequestContext34GetResultWithInteractionIfRequiredEv" => fail,
    "_ZN2nn7account42NintendoAccountAuthorizationRequestContext10GetIdTokenEPmPcm" => token_member,
    "_ZN2nn7account42NintendoAccountAuthorizationRequestContext20GetAuthorizationCodeEPmPcm" => token_member,
    "_ZN2nn7account42NintendoAccountAuthorizationRequestContext8GetStateEPmPcm" => token_member,
    "_ZN2nn7account12GetUserCountEPi" => user_count,
    "_ZN2nn7account18ListQualifiedUsersEPiPNS0_3UidEi" => list_users,
    "_ZN2nn7account16GetUserExistenceEPbRKNS0_3UidE" => user_existence,
    "_ZN2nn7account15ShowUserCreatorEv" => show_creator,
    "_ZN2nn7account16LoadProfileImageEPmPvmRKNS0_3UidE" => load_profile_image,
    "_ZN2nn7account27StoreSaveDataThumbnailImageERKNS0_3UidEPKvm" => ok,
    "_ZN2nn7account28DeleteSaveDataThumbnailImageERKNS0_3UidE" => ok,
    "_ZN2nn7account32IsNetworkServiceAccountAvailableEPbRKNS0_10UserHandleE" => network_account_available,
    "_ZN2nn7account9CloseUserERKNS0_10UserHandleE" => nothing,
    "_ZN2nn7account12AsyncContext6CancelEv" => ok,
    "_ZN2nn7account12AsyncContext7HasDoneEPb" => has_done,
    "_ZN2nn7account12AsyncContextD1Ev" => nothing,
    "_ZN2nn7account12AsyncContextaSEOS1_" => move_assign,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_handle_tag_spells_userhndl() {
        assert_eq!(&HANDLE_MAGIC.to_be_bytes(), b"USERHNDL");
    }
}
