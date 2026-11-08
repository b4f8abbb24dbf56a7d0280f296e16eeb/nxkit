//! `nn::sf::hipc` and the camera context: host sessions for services that the game opens through the SDK. Source:
//! nxnative `dm/host/hle_sf.c`.
//!
//! A game that links the HIPC client of the SDK connects to a service by name. `ConnectToHipcService` gives a host
//! session for a name in [`Config::sf_services`](crate::Config::sf_services) and fails with the result of an
//! unregistered service for any other name. The title module lists the names that its game uses. A host session
//! answers every request with success and zeroed output, and it parses only the HIPC and CMIF envelope in
//! `SendSyncRequest`. The session handles are 0xd0000 plus the slot, outside the kernel handle table.
//!
//! The camera context is a host session whose commands 3 and 4 succeed and whose command 21 gives a readable event. Any
//! other command fails with 0x7c4, no camera device.
//!
//! Every slot is a set of atomics. `SendSyncRequest` takes no lock. A slot is claimed with a compare-exchange.

use super::{get_u32, log, log_once_with, ok, put_u16, put_u32, put_zero};
use crate::config;
use nxkit_abi::kernel::kernel;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

const HANDLE_BASE: u32 = 0xd0000;
const SLOTS: usize = 64;
const NOT_REGISTERED: u32 = 0xe15;
const INVALID_HANDLE: u32 = 0xe401;
const SESSION_CLOSED: u32 = 0xf601;
const NO_CAMERA: u32 = 0x7c4;
const SFCI: u32 = 0x4943_4653;
const SFCO: u32 = 0x4f43_4653;

struct Session {
    used: AtomicBool,
    /// The service name, at most 8 bytes, packed little endian.
    name: AtomicU64,
    domain: AtomicU32,
    /// The last object and command that were logged, as `object << 16 | command`, plus one.
    seen: AtomicU32,
    camera: AtomicBool,
}

static SESSIONS: [Session; SLOTS] = [const {
    Session {
        used: AtomicBool::new(false),
        name: AtomicU64::new(0),
        domain: AtomicU32::new(0),
        seen: AtomicU32::new(0),
        camera: AtomicBool::new(false),
    }
}; SLOTS];

fn pack(name: &str) -> u64 {
    let mut b = [0u8; 8];
    for (d, s) in b.iter_mut().zip(name.bytes()) {
        *d = s;
    }
    u64::from_le_bytes(b)
}

fn slot(handle: u32) -> Option<&'static Session> {
    let s = SESSIONS.get(handle.checked_sub(HANDLE_BASE)? as usize)?;
    s.used.load(Ordering::Acquire).then_some(s)
}

fn new_session(name: u64, domain: u32, camera: bool) -> u32 {
    for (i, s) in SESSIONS.iter().enumerate() {
        if s.used
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            s.name.store(name, Ordering::Release);
            s.domain.store(domain, Ordering::Release);
            s.seen.store(0, Ordering::Release);
            s.camera.store(camera, Ordering::Release);
            return HANDLE_BASE + i as u32;
        }
    }
    super::fatal(&format!("sf: more than {SLOTS} host service sessions"))
}

/// `CreateCameraContext(UniqueNativeHandle*)`: a session without a camera.
extern "C" fn create_camera_context(out: u64) -> u32 {
    let h = new_session(pack("camera"), 0, true);
    put_u32(out, h);
    log(&format!(
        "sf: camera context, host session {h:#x} (no camera)"
    ));
    0
}

/// `ConnectToHipcService(Handle*, const char* name)`.
extern "C" fn connect(out: u64, name: u64) -> u32 {
    let given = super::get_cstr(name, 8);
    let served = config()
        .sf_services
        .iter()
        .find(|s| given.as_bytes().starts_with(s.as_bytes()) && s.len() <= 8);
    match served {
        Some(s) => {
            let h = new_session(pack(s), 0, false);
            put_u32(out, h);
            0
        }
        None => {
            log(&format!(
                "sf: the game connects to service {given} itself, which the host does not serve"
            ));
            NOT_REGISTERED
        }
    }
}

extern "C" fn close_session(handle: u32) {
    match slot(handle) {
        Some(s) => s.used.store(false, Ordering::Release),
        None => log(&format!(
            "sf: CloseClientSessionHandle on {handle:#x}, not a host session"
        )),
    }
}

/// `QueryPointerBufferSize(uint16_t*, Handle)`: no pointer buffer.
extern "C" fn pointer_buffer(out: u64, handle: u32) -> u32 {
    if slot(handle).is_none() {
        return super::RES_FAIL;
    }
    put_u16(out, 0);
    0
}

/// `ConvertSessionToDomain(CmifDomainObjectId*, Handle)`: the session object is object 1.
extern "C" fn to_domain(out: u64, handle: u32) -> u32 {
    let Some(s) = slot(handle) else {
        return super::RES_FAIL;
    };
    s.domain.store(1, Ordering::Release);
    put_u32(out, 1);
    0
}

/// `DuplicateSession(Handle*, Handle)` and `DuplicateSessionEx(Handle*, Handle, unsigned)`.
extern "C" fn duplicate(out: u64, handle: u32) -> u32 {
    let Some(s) = slot(handle) else {
        return super::RES_FAIL;
    };
    let h = new_session(
        s.name.load(Ordering::Acquire),
        s.domain.load(Ordering::Acquire),
        s.camera.load(Ordering::Acquire),
    );
    put_u32(out, h);
    0
}

/// What a session answers. The result, and a handle to copy into the reply.
fn answer(s: &Session, cmd: u32) -> (u32, u32) {
    if !s.camera.load(Ordering::Acquire) {
        return (0, 0);
    }
    match cmd {
        3 | 4 => (0, 0),
        21 => match kernel().and_then(|k| k.create_event().ok()) {
            Some((_, readable)) => (0, readable),
            None => (NO_CAMERA, 0),
        },
        29 => (NO_CAMERA, 0),
        _ => {
            log_once_with(&CAMERA_LOG, || {
                format!("sf: camera command {cmd} answered {NO_CAMERA:#x} (no camera device)")
            });
            (NO_CAMERA, 0)
        }
    }
}

static CAMERA_LOG: AtomicBool = AtomicBool::new(false);

/// `SendSyncRequest(Handle, void* message, size_t size)`: the host answers the request of a host session. The reply
/// replaces the message in place.
extern "C" fn send_sync_request(handle: u32, msg: u64, size: u64) -> u32 {
    let Some(s) = slot(handle) else {
        return INVALID_HANDLE;
    };
    if size < 0x40 {
        return INVALID_HANDLE;
    }
    let w0 = get_u32(msg);
    let w1 = get_u32(msg + 4);
    let kind = w0 & 0xffff;
    let nx = (w0 >> 16) & 15;
    let nabw = ((w0 >> 20) & 15) + ((w0 >> 24) & 15) + ((w0 >> 28) & 15);
    let ndw = w1 & 0x3ff;
    let mut p = msg + 8;
    if w1 >> 31 != 0 {
        let sh = get_u32(p);
        p += 4 + 4 * (if sh & 1 != 0 { 2 } else { 0 } + ((sh >> 1) & 15) + ((sh >> 5) & 15)) as u64;
    }
    p += u64::from(nx) * 8 + u64::from(nabw) * 12;
    let data = p - msg;
    let cmif = (data + 15) & !15;
    let domain = s.domain.load(Ordering::Acquire) != 0 && (kind == 4 || kind == 6);
    let dtype = if domain { super::get_u8(msg + cmif) } else { 1 };
    let object = if domain { get_u32(msg + cmif + 4) } else { 0 };
    let header = cmif + if domain { 16 } else { 0 };
    if header + 16 > size || data + u64::from(ndw) * 4 > size {
        return INVALID_HANDLE;
    }
    let (magic, version, cmd) = (
        get_u32(msg + header),
        get_u32(msg + header + 4),
        get_u32(msg + header + 8),
    );
    if kind == 2 {
        return SESSION_CLOSED;
    }
    if kind != 4 && kind != 6 {
        return INVALID_HANDLE;
    }
    if dtype == 2 {
        put_zero(msg, 16);
        return 0;
    }
    if magic != SFCI {
        return INVALID_HANDLE;
    }
    let (result, copy) = answer(s, cmd & 0xffff);
    let end = size.min(0x1000);
    let words = if copy != 0 { 0x10 } else { 8 };
    put_zero(msg, end);
    let mut head = ((end - words) / 4).min(0x3ff) as u32;
    if copy != 0 {
        head |= 1 << 31;
        put_u32(msg + 8, 1 << 1);
        put_u32(msg + 12, copy);
    }
    put_u32(msg + 4, head);
    let out = msg + 0x10 + if domain { 16 } else { 0 };
    put_u32(out, SFCO);
    put_u32(out + 4, version);
    put_u32(out + 8, result);
    let key = (object & 0xff) << 16 | (cmd & 0xffff);
    if s.seen.swap(key + 1, Ordering::AcqRel) != key + 1 {
        log(&format!(
            "sf: object {object} command {cmd}: {}",
            if result == 0 { "success" } else { "fails" }
        ));
    }
    0
}

table! {
    "_ZN2nn6camera3sdk20DestroyCameraContextEv" => ok,
    "_ZN2nn6applet24BeginUsingCameraResourceEv" => ok,
    "_ZN2nn6applet22EndUsingCameraResourceEv" => ok,
    "_ZN2nn6camera3sdk19CreateCameraContextEPNS_5gsdrv18UniqueNativeHandleE" => create_camera_context,
    "_ZN2nn2sf4hipc31InitializeHipcServiceResolutionEv" => ok,
    "_ZN2nn2sf4hipc29FinalizeHipcServiceResolutionEv" => super::nothing,
    "_ZN2nn2sf4hipc20ConnectToHipcServiceEPNS_3svc6HandleEPKc" => connect,
    "_ZN2nn2sf4hipc24CloseClientSessionHandleENS_3svc6HandleE" => close_session,
    "_ZN2nn2sf4hipc6client22QueryPointerBufferSizeEPtNS_3svc6HandleE" => pointer_buffer,
    "_ZN2nn2sf4hipc6client22ConvertSessionToDomainEPNS0_4cmif18CmifDomainObjectIdENS_3svc6HandleE" => to_domain,
    "_ZN2nn2sf4hipc6client16DuplicateSessionEPNS_3svc6HandleES4_" => duplicate,
    "_ZN2nn2sf4hipc6client18DuplicateSessionExEPNS_3svc6HandleES4_j" => duplicate,
    "_ZN2nn2sf4hipc15SendSyncRequestENS_3svc6HandleEPvm" => send_sync_request,
}
