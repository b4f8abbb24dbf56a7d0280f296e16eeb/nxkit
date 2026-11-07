//! `nn::chat` take-overs (GameChat of the Switch 2 SDKs). Source: nxnative `dm/host/hle_misc.c`, the chat section.
//!
//! The host has no GameChat. The library is ready and no chat runs, so nobody is chatting, there is no room, and the
//! camera and sharing calls succeed and do nothing. The SDK versions go through a service object that `Initialize`
//! would have made, so each function a game imports needs an export here.
//!
//! The camera video storage holds the images of the camera, each a native image that the game makes a texture from.
//! The host has no camera, so each image is a blank native image of the size that the `native_image_blank` seam makes.
//! The storage type of the game (0xe0 bytes) keeps `{u32 magic, u32 count, u32 image[4]}`.

use super::{RES_FAIL, fail, get_u32, log, log_once, nothing, ok, put_u32, put_zero, zero};
use crate::event;
use crate::seams::seams;
use std::sync::atomic::AtomicBool;

const STORAGE_LEN: u64 = 0xe0;
const MAGIC: u32 = 0x6d43_6d44;
const MAX_IMAGES: u32 = 4;

static NO_ROOM: AtomicBool = AtomicBool::new(false);
static NO_WINDOW: AtomicBool = AtomicBool::new(false);

/// `nn::account::Uid`, returned in two registers.
#[repr(C)]
struct Uid {
    a: u64,
    b: u64,
}

/// `AttachToRoomInfoChangedEvent(SystemEvent*)`: bound and never signalled.
extern "C" fn room_event(ev: u64) -> u32 {
    event::bind(ev, false);
    0
}

/// `Calculate{Local,Remote}CameraVideoBufferSize(int, int, int)`: one page. The host keeps the images itself.
extern "C" fn buffer_size() -> u64 {
    0x1000
}

fn storage_init(ty: u64, w: u32, h: u32, n: u32) {
    put_zero(ty, STORAGE_LEN);
    let (w, h, n) = (w as i32, h as i32, n as i32);
    if !(32..=1920).contains(&w) || !(32..=1080).contains(&h) || !(1..=MAX_IMAGES as i32).contains(&n) {
        log(&format!("chat: camera video storage {w}x{h} x{n} is out of the SDK range"));
        return;
    }
    let Some(blank) = seams().native_image_blank else {
        return;
    };
    put_u32(ty, MAGIC);
    put_u32(ty + 4, n as u32);
    for i in 0..n as u64 {
        put_u32(ty + 8 + 4 * i, blank(w as u32, h as u32));
    }
}

fn storage_fini(ty: u64) {
    if ty == 0 || get_u32(ty) != MAGIC {
        return;
    }
    if let Some(free) = seams().native_image_free {
        for i in 0..u64::from(get_u32(ty + 4).min(MAX_IMAGES)) {
            free(get_u32(ty + 8 + 4 * i));
        }
    }
    put_zero(ty, STORAGE_LEN);
}

/// `Initialize{Local,Remote}CameraVideoStorage(type*, int w, int h, int n, void*, size_t)`.
extern "C" fn video_init(ty: u64, w: u32, h: u32, n: u32) {
    storage_init(ty, w, h, n);
}

extern "C" fn video_fini(ty: u64) {
    storage_fini(ty);
}

/// `Get{Local,Remote}CameraImageHandle(const type*, int)`.
extern "C" fn image_handle(ty: u64, i: u32) -> u32 {
    if ty == 0 || get_u32(ty) != MAGIC || i >= get_u32(ty + 4) || i >= MAX_IMAGES {
        return 0;
    }
    get_u32(ty + 8 + 4 * u64::from(i))
}

extern "C" fn chatting_uid() -> Uid {
    Uid { a: 0, b: 0 }
}

/// `GetRoomInfo(RoomInfo*)`: no room. The library result is not known, and a game reads any failure as no room.
extern "C" fn room_info() -> u32 {
    log_once(&NO_ROOM, "chat: GetRoomInfo with no chat room fails");
    RES_FAIL
}

/// `GetVirtualCameraNativeWindowHandle`: no window.
extern "C" fn virtual_window() -> u64 {
    log_once(&NO_WINDOW, "chat: GetVirtualCameraNativeWindowHandle has no window");
    0
}

table! {
    "_ZN2nn4chat10InitializeEv" => nothing,
    "_ZN2nn4chat10IsChattingEv" => zero,
    "_ZN2nn4chat20SetScreenSharingModeENS0_17ScreenSharingModeE" => nothing,
    "_ZN2nn4chat28AttachToRoomInfoChangedEventEPNS_2os11SystemEventE" => room_event,
    "_ZN2nn4chat35CalculateLocalCameraVideoBufferSizeEiii" => buffer_size,
    "_ZN2nn4chat36CalculateRemoteCameraVideoBufferSizeEiii" => buffer_size,
    "_ZN2nn4chat33InitializeLocalCameraVideoStorageEPNS0_20LocalCameraVideoTypeEiiiPvm" => video_init,
    "_ZN2nn4chat34InitializeRemoteCameraVideoStorageEPNS0_21RemoteCameraVideoTypeEiiiPvm" => video_init,
    "_ZN2nn4chat31FinalizeLocalCameraVideoStorageEPNS0_20LocalCameraVideoTypeE" => video_fini,
    "_ZN2nn4chat32FinalizeRemoteCameraVideoStorageEPNS0_21RemoteCameraVideoTypeE" => video_fini,
    "_ZN2nn4chat25GetLocalCameraImageHandleEPKNS0_20LocalCameraVideoTypeEi" => image_handle,
    "_ZN2nn4chat26GetRemoteCameraImageHandleEPKNS0_21RemoteCameraVideoTypeEi" => image_handle,
    "_ZN2nn4chat14GetChattingUidEv" => chatting_uid,
    "_ZN2nn4chat11GetRoomInfoEPNS0_8RoomInfoE" => room_info,
    "_ZN2nn4chat13SetSharingKeyERKNS0_10SharingKeyE" => ok,
    "_ZN2nn4chat15ClearSharingKeyEv" => nothing,
    "_ZN2nn4chat13ShowChatStartERKNS_7account3UidE" => ok,
    "_ZN2nn4chat22ShowCameraSharingGuideEv" => ok,
    "_ZN2nn4chat28ShowLocalCameraConfigurationENS0_25CameraImageProcessingModeE" => ok,
    "_ZN2nn4chat44GetCameraImageProcessingModeForVirtualCameraEv" => zero,
    "_ZN2nn4chat22GetRealtimeAudioLevelsEPNS0_10AudioLevelEiPKNS_7account23NetworkServiceAccountIdEi" => zero,
    "_ZN2nn4chat23InitializeVirtualCameraEPNS0_17VirtualCameraTypeE" => ok,
    "_ZN2nn4chat21FinalizeVirtualCameraEPNS0_17VirtualCameraTypeE" => nothing,
    "_ZN2nn4chat19EnableVirtualCameraEPNS0_17VirtualCameraTypeE" => ok,
    "_ZN2nn4chat20DisableVirtualCameraEPNS0_17VirtualCameraTypeE" => ok,
    "_ZN2nn4chat43SetVirtualCameraContainPhotographedContentsEPNS0_17VirtualCameraTypeEb" => nothing,
    "_ZN2nn4chat34GetVirtualCameraNativeWindowHandleEPKNS0_17VirtualCameraTypeE" => virtual_window,
    "_ZN2nn4chat25StartLocalCameraStreamingEv" => ok,
    "_ZN2nn4chat24StopLocalCameraStreamingEv" => ok,
    "_ZN2nn4chat26StartRemoteCameraStreamingERKNS_7account23NetworkServiceAccountIdE" => ok,
    "_ZN2nn4chat25StopRemoteCameraStreamingERKNS_7account23NetworkServiceAccountIdE" => ok,
    "_ZN2nn4chat27RequestNextLocalCameraImageEPNS_2gs16NativeSyncHandleEPNS0_20LocalCameraVideoTypeEiS2_" => fail,
    "_ZN2nn4chat28RequestNextRemoteCameraImageEPNS_2gs16NativeSyncHandleEPNS0_21RemoteCameraVideoTypeEiS2_RKNS_7account23NetworkServiceAccountIdE" => fail,
    "_ZN2nn4chat46BeginFullScreenWithLocalCameraAvailableSectionEb" => ok,
    "_ZN2nn4chat44EndFullScreenWithLocalCameraAvailableSectionEv" => nothing,
}
