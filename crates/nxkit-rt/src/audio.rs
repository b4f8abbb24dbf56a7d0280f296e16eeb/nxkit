//! The `audio` service seam. The host passes C callbacks. This module turns them into the [`nxkit_svc_audio::Host`] that the
//! service calls, and applies the settings.

use nxkit_svc_audio::{Config, Host};
use std::ffi::{CString, c_char};
use std::sync::OnceLock;

/// The callbacks of the host that the audio take-overs need. Every field is required. The names are those of the host
/// functions that the engine has (`guest_sym`, `hle_bind_system_event`, `hle_signal_system_event`,
/// `hle_dup_readable_handle`, `dm_guest_thread_spawn`, `dm_guest_thread_join` and `die`).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NxkitAudioHost {
    /// The address of the SDK's own function with this NUL-terminated mangled name, or 0.
    pub guest_sym: Option<extern "C" fn(*const c_char) -> usize>,
    /// Binds the guest `SystemEvent` at this address to a new kernel event. The second argument is 1 when the guest
    /// object owns the handle.
    pub bind_system_event: Option<extern "C" fn(u64, i32)>,
    /// Signals the guest `SystemEvent` at this address.
    pub signal_system_event: Option<extern "C" fn(u64)>,
    /// Copies the readable handle of a system event for the guest to own.
    pub dup_readable_handle: Option<extern "C" fn(u32) -> u32>,
    /// Starts a guest thread that runs `entry(arg)` with a priority and a stack size. Returns its thread object.
    pub spawn_guest_thread: Option<extern "C" fn(extern "C" fn(u64), u64, i32, usize) -> u64>,
    /// Waits until the guest thread ends.
    pub join_guest_thread: Option<extern "C" fn(u64)>,
    /// Stops the game with this NUL-terminated message. It does not return.
    pub fatal: Option<extern "C" fn(*const c_char)>,
}

/// The settings of the audio service. They come from `game.json`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NxkitAudioConfig {
    /// The gain of the final mix. 1.0 leaves it unchanged.
    pub master_volume: f32,
    /// 1 drops the mix and opens no output device. The game still sees its buffers played.
    pub silent_sink: i32,
}

/// The callbacks, all present.
#[derive(Clone, Copy)]
struct Cb {
    guest_sym: extern "C" fn(*const c_char) -> usize,
    bind_system_event: extern "C" fn(u64, i32),
    signal_system_event: extern "C" fn(u64),
    dup_readable_handle: extern "C" fn(u32) -> u32,
    spawn_guest_thread: extern "C" fn(extern "C" fn(u64), u64, i32, usize) -> u64,
    join_guest_thread: extern "C" fn(u64),
    fatal: extern "C" fn(*const c_char),
}

static CB: OnceLock<Cb> = OnceLock::new();

fn guest_sym(name: &str) -> usize {
    let (Some(cb), Ok(c)) = (CB.get(), CString::new(name)) else {
        return 0;
    };
    (cb.guest_sym)(c.as_ptr())
}

fn bind_system_event(addr: u64, managed: bool) {
    if let Some(cb) = CB.get() {
        (cb.bind_system_event)(addr, i32::from(managed));
    }
}

fn signal_system_event(addr: u64) {
    if let Some(cb) = CB.get() {
        (cb.signal_system_event)(addr);
    }
}

fn dup_readable_handle(h: u32) -> u32 {
    CB.get().map_or(0, |cb| (cb.dup_readable_handle)(h))
}

fn spawn_guest_thread(entry: extern "C" fn(u64), arg: u64, prio: i32, stack: usize) -> u64 {
    CB.get()
        .map_or(0, |cb| (cb.spawn_guest_thread)(entry, arg, prio, stack))
}

fn join_guest_thread(t: u64) {
    if let Some(cb) = CB.get() {
        (cb.join_guest_thread)(t);
    }
}

fn fatal(msg: &str) {
    if let (Some(cb), Ok(c)) = (CB.get(), CString::new(msg)) {
        (cb.fatal)(c.as_ptr());
    }
}

/// Gives the audio service its host callbacks and settings. Call it once before the game starts and before the
/// service is enabled in [`crate::nxkit_init`]. `cfg` may be NULL for the defaults. Returns 1 on success and 0 when a
/// callback is missing or the callbacks were set before.
///
/// # Safety
///
/// `host` is NULL or points to a valid [`NxkitAudioHost`]. `cfg` is NULL or points to a valid [`NxkitAudioConfig`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nxkit_audio_init(
    host: *const NxkitAudioHost,
    cfg: *const NxkitAudioConfig,
) -> i32 {
    // SAFETY: the caller promises that a non-NULL `host` is valid.
    let Some(h) = (unsafe { host.as_ref() }) else {
        return 0;
    };
    let (Some(a), Some(b), Some(c), Some(d), Some(e), Some(f), Some(g)) = (
        h.guest_sym,
        h.bind_system_event,
        h.signal_system_event,
        h.dup_readable_handle,
        h.spawn_guest_thread,
        h.join_guest_thread,
        h.fatal,
    ) else {
        return 0;
    };
    let cb = Cb {
        guest_sym: a,
        bind_system_event: b,
        signal_system_event: c,
        dup_readable_handle: d,
        spawn_guest_thread: e,
        join_guest_thread: f,
        fatal: g,
    };
    if CB.set(cb).is_err() {
        return 0;
    }
    let ok = nxkit_svc_audio::set_host(Host {
        guest_sym,
        bind_system_event,
        signal_system_event,
        dup_readable_handle,
        spawn_guest_thread,
        join_guest_thread,
        fatal,
    });
    // SAFETY: the caller promises that a non-NULL `cfg` is valid.
    let config = match unsafe { cfg.as_ref() } {
        Some(c) => Config {
            master_volume: c.master_volume,
            silent_sink: c.silent_sink != 0,
        },
        None => Config::default(),
    };
    nxkit_svc_audio::init(&config);
    i32::from(ok)
}
