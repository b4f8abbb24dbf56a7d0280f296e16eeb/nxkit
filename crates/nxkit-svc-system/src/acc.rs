//! The `acc:u0` and `acc:u1` services: `IAccountServiceForApplication`, `IProfile`, `IManagerForApplication` and the
//! async context of the online calls.
//!
//! IDs and layouts are from libnx `acc.c` and switchbrew `Account services`. The values follow the engine
//! `dm/host/hle_account.c`:
//! - The registered local users of `users.rs`. User 1 has the uid of the one user before, and the nickname
//!   `Config::nickname`. The others are `Player <n>`. The preselected user is the launch user.
//! - The profile image is the plain 256x256 JPEG of the engine (`k_profile_jpeg`, 670 bytes).
//! - Every online call fails with `RES_FAIL` (the network service account is out of scope).
//! - Save data thumbnails are accepted and dropped.
//!
//! Root commands: 0 `GetUserCount`, 1 `GetUserExistence`, 2 `ListAllUsers`, 3 `ListOpenUsers`, 4 `GetLastOpenedUser`, 5
//! `GetProfile`, 50 `IsUserRegistrationRequestPermitted`, 51 and 52 `TrySelectUserWithoutInteraction`, 100 and 140
//! `InitializeApplicationInfo`, 101 `GetBaasAccountManagerForApplication`, 102 `AuthenticateApplicationAsync`, 103
//! `CheckNetworkServiceAvailabilityAsync`, 110 `StoreSaveDataThumbnail`, 111 `ClearSaveDataThumbnail`, 141
//! `ListQualifiedUsers` and 150 `IsUserAccountSwitchLocked`.
//!
//! `IProfile`: 0 `Get`, 1 `GetBase`, 10 `GetImageSize`, 11 `LoadImage`.
//!
//! `IManagerForApplication`: 0 `CheckAvailability` gives false, 160 `StoreOpenContext` succeeds, and 1
//! `GetAccountId`, 2 `EnsureIdTokenCacheAsync`, 3 `LoadIdTokenCache`, 130
//! `GetNintendoAccountUserResourceCacheForApplication`, 150 `CreateAuthorizationRequest` and 170
//! `LoadNetworkServiceLicenseKindAsync` fail.
//!
//! The async context: 0 `GetSystemEvent` (signalled at once), 1 `Cancel`, 2 `HasDone` (true) and 3 `GetResult` (fails),
//! as `AsyncContext`.

use crate::config;
use crate::users::{self, users};
use crate::util::{Event, RES_FAIL, fail, padded, write_out};
use nxkit_abi::ipc::{Handled, Request, Response, Service};
use std::sync::Arc;

/// The slots of a uid list in the guest.
const LIST_SLOTS: usize = 8;

/// The profile image of the engine: a baseline JPEG of 256x256 pixels.
const JPEG_HEAD: [u8; 285] = [
    0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00, 0x00, 0x01,
    0x00, 0x01, 0x00, 0x00, 0xff, 0xdb, 0x00, 0x43, 0x00, 0x08, 0x06, 0x06, 0x07, 0x06, 0x05, 0x08,
    0x07, 0x07, 0x07, 0x09, 0x09, 0x08, 0x0a, 0x0c, 0x14, 0x0d, 0x0c, 0x0b, 0x0b, 0x0c, 0x19, 0x12,
    0x13, 0x0f, 0x14, 0x1d, 0x1a, 0x1f, 0x1e, 0x1d, 0x1a, 0x1c, 0x1c, 0x20, 0x24, 0x2e, 0x27, 0x20,
    0x22, 0x2c, 0x23, 0x1c, 0x1c, 0x28, 0x37, 0x29, 0x2c, 0x30, 0x31, 0x34, 0x34, 0x34, 0x1f, 0x27,
    0x39, 0x3d, 0x38, 0x32, 0x3c, 0x2e, 0x33, 0x34, 0x32, 0xff, 0xdb, 0x00, 0x43, 0x01, 0x09, 0x09,
    0x09, 0x0c, 0x0b, 0x0c, 0x18, 0x0d, 0x0d, 0x18, 0x32, 0x21, 0x1c, 0x21, 0x32, 0x32, 0x32, 0x32,
    0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32,
    0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32,
    0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0xff, 0xc0,
    0x00, 0x11, 0x08, 0x01, 0x00, 0x01, 0x00, 0x03, 0x01, 0x22, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11,
    0x01, 0xff, 0xc4, 0x00, 0x15, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0xff, 0xc4, 0x00, 0x14, 0x10, 0x01, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xc4,
    0x00, 0x15, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x05, 0xff, 0xc4, 0x00, 0x14, 0x11, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xda, 0x00, 0x0c, 0x03,
    0x01, 0x00, 0x02, 0x11, 0x03, 0x11, 0x00, 0x3f, 0x00, 0x8e, 0x02, 0xe2, 0x50,
];

/// The zero bytes of the entropy-coded data between the head and the end of the image.
const JPEG_ZEROS: usize = 382;

/// The end of the image.
const JPEG_TAIL: [u8; 3] = [0x3f, 0xff, 0xd9];

/// The profile image.
pub(crate) fn profile_jpeg() -> Vec<u8> {
    let mut v = Vec::with_capacity(JPEG_HEAD.len() + JPEG_ZEROS + JPEG_TAIL.len());
    v.extend_from_slice(&JPEG_HEAD);
    v.resize(JPEG_HEAD.len() + JPEG_ZEROS, 0);
    v.extend_from_slice(&JPEG_TAIL);
    v
}

fn uid_bytes(slot: usize) -> [u8; 16] {
    let uid = users::uid(slot);
    let mut b = [0u8; 16];
    b[..8].copy_from_slice(&uid[0].to_le_bytes());
    b[8..].copy_from_slice(&uid[1].to_le_bytes());
    b
}

/// A uid from the request data at `offset`.
fn uid_at(req: &Request<'_>, offset: usize) -> Option<[u64; 2]> {
    Some([req.u64_at(offset)?, req.u64_at(offset + 8)?])
}

/// The uid list of the guest: the users, then zeros up to the size of the buffer (at most 8 slots).
fn uid_list(slots: &[usize]) -> Vec<u8> {
    let mut v = vec![0u8; 16 * LIST_SLOTS];
    for (i, &slot) in slots.iter().take(LIST_SLOTS).enumerate() {
        v[16 * i..16 * i + 16].copy_from_slice(&uid_bytes(slot));
    }
    v
}

/// `ProfileBase` of a user: the uid, the time of the last edit and the nickname.
fn profile_base(slot: usize) -> [u8; 0x38] {
    let mut b = [0u8; 0x38];
    b[..16].copy_from_slice(&uid_bytes(slot));
    b[24..].copy_from_slice(&padded::<0x20>(&users::nickname(slot, &config().nickname)));
    b
}

/// `acc:u0` and `acc:u1`.
pub(crate) struct Account {
    pub(crate) name: &'static str,
}

impl Service for Account {
    fn name(&self) -> &'static str {
        self.name
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            0 => rsp.push_u32(users().registered().len() as u32),
            1 => rsp.push_bytes(&[u8::from(
                uid_at(req, 0).and_then(|u| users().user(u)).is_some(),
            )]),
            2 | 141 => {
                let _ = write_out(req, 0, &uid_list(&users().registered()));
            }
            3 => {
                let _ = write_out(req, 0, &uid_list(&users().opened()));
            }
            4 => rsp.push_bytes(&uid_bytes(users().last())),
            51 | 52 => rsp.push_bytes(&uid_bytes(users().launch())),
            5 => {
                let Some(slot) = uid_at(req, 0).and_then(|u| users().user(u)) else {
                    return fail(rsp, RES_FAIL);
                };
                rsp.objects.push(Arc::new(Profile { slot }));
            }
            50 | 150 => rsp.push_bytes(&[0]),
            100 | 140 | 110 | 111 => {}
            101 => rsp.objects.push(Arc::new(Manager)),
            102 | 103 => rsp.objects.push(Arc::new(AsyncContext {
                event: Event::new(),
            })),
            _ => return Handled::Unknown,
        }
        Handled::Done
    }
}

/// `nn::account::detail::IProfile` of one user.
struct Profile {
    slot: usize,
}

impl Service for Profile {
    fn name(&self) -> &'static str {
        "acc::IProfile"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            0 => {
                let _ = write_out(req, 0, &[0u8; 0x80]);
                rsp.push_bytes(&profile_base(self.slot));
            }
            1 => rsp.push_bytes(&profile_base(self.slot)),
            10 => rsp.push_u32(profile_jpeg().len() as u32),
            11 => {
                let image = profile_jpeg();
                let written = write_out(req, 0, &image);
                if written.is_none() {
                    return fail(rsp, RES_FAIL);
                }
                rsp.push_u32(image.len() as u32);
            }
            _ => return Handled::Unknown,
        }
        Handled::Done
    }
}

/// `nn::account::detail::IManagerForApplication`.
struct Manager;

impl Service for Manager {
    fn name(&self) -> &'static str {
        "acc::IManagerForApplication"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            0 => {
                rsp.push_bytes(&[0]);
                Handled::Done
            }
            160 => Handled::Done,
            1 | 2 | 3 | 130 | 150 | 170 => fail(rsp, RES_FAIL),
            _ => Handled::Unknown,
        }
    }
}

/// `nn::account::detail::IAsyncContext` of an online call.
struct AsyncContext {
    event: Event,
}

impl Service for AsyncContext {
    fn name(&self) -> &'static str {
        "acc::IAsyncContext"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            0 => self.event.reply(rsp, true),
            1 => Handled::Done,
            2 => {
                rsp.push_bytes(&[1]);
                Handled::Done
            }
            3 => fail(rsp, RES_FAIL),
            _ => Handled::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_profile_image_has_the_expected_size() {
        let image = profile_jpeg();
        assert_eq!(image.len(), 670);
        assert_eq!(&image[..2], &[0xff, 0xd8]);
        assert_eq!(&image[image.len() - 2..], &[0xff, 0xd9]);
    }

    #[test]
    fn the_uid_has_two_words() {
        let b = uid_bytes(0);
        assert_eq!(&b[..8], &0x7862_6c00_0000_0001u64.to_le_bytes());
        assert_eq!(&b[8..], &0x5357_4e41_5449_5645u64.to_le_bytes());
    }

    #[test]
    fn the_profile_base_ends_with_the_nickname() {
        assert_eq!(&profile_base(0)[24..30], b"Player");
        assert_eq!(&profile_base(1)[24..32], b"Player 2");
    }

    #[test]
    fn the_uid_list_holds_the_users_in_order() {
        let v = uid_list(&[0, 2]);
        assert_eq!(&v[..16], &uid_bytes(0));
        assert_eq!(&v[16..32], &uid_bytes(2));
        assert!(v[32..].iter().all(|b| *b == 0));
    }
}
