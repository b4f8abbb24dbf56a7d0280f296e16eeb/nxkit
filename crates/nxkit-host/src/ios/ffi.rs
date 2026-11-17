use crate::api::{Paths, Touch};
use crate::session::GameSession;
use objc2::{MainThreadMarker, rc::Retained};
use objc2_quartz_core::CAMetalLayer;
use std::{
    cell::RefCell,
    ffi::{CStr, c_char, c_void},
    path::Path,
};

thread_local! {
    static SESSION: RefCell<Option<GameSession>> = const { RefCell::new(None) };
}
fn main_thread() -> bool {
    MainThreadMarker::new().is_some()
}
fn with_session(f: impl FnOnce(&mut GameSession)) {
    if main_thread() {
        SESSION.with_borrow_mut(|s| {
            if let Some(s) = s {
                f(s);
            }
        });
    }
}

// SAFETY: The checked-in C header and Swift caller provide valid NUL-terminated
// strings, a live CAMetalLayer, and writable error_capacity bytes (or null/zero).
// Pointers are borrowed only for this call; CAMetalLayer is explicitly retained.
/// Starts the bundled game on the UIKit main thread.
///
/// # Safety
/// `layer` must be a live CAMetalLayer. Paths must be valid NUL-terminated UTF-8 strings.
/// `error` must be writable for `error_capacity` bytes when non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nxkit_start(
    layer: *mut c_void,
    bundle: *const c_char,
    home: *const c_char,
    maximum_fps: u32,
    error: *mut c_char,
    error_capacity: usize,
) -> bool {
    let result = (|| {
        if !main_thread() {
            return Err("Runtime calls require UIKit's main thread".into());
        }
        if layer.is_null() || bundle.is_null() || home.is_null() {
            return Err("Missing native session inputs".into());
        }
        // SAFETY: non-null borrowed strings have the lifetime guaranteed above.
        let bundle = unsafe { CStr::from_ptr(bundle) }
            .to_str()
            .map_err(|_| "Invalid bundle path")?;
        let home = unsafe { CStr::from_ptr(home) }
            .to_str()
            .map_err(|_| "Invalid sandbox path")?;
        let paths = Paths::new(Path::new(bundle), Path::new(home))?;
        paths.prepare()?;
        // SAFETY: Swift passed the actual CAMetalLayer backing its live UIView.
        let layer = unsafe { Retained::retain(layer.cast::<CAMetalLayer>()) }
            .ok_or("Missing Metal layer")?;
        let session = crate::session::start(layer, paths, maximum_fps)?;
        SESSION.with_borrow_mut(|s| *s = Some(session));
        Ok::<(), String>(())
    })();
    if !error.is_null() && error_capacity > 0 {
        // SAFETY: the caller provided the writable buffer length per C contract.
        let out = unsafe { std::slice::from_raw_parts_mut(error.cast(), error_capacity) };
        super::error_bytes(result.as_ref().err().map_or("", String::as_str), out);
    }
    result.is_ok()
}
/// Updates foreground input, audio and rendering state.
#[unsafe(no_mangle)]
pub extern "C" fn nxkit_set_active(active: bool) {
    with_session(|s| s.set_active(active));
}
/// Updates the drawable size in pixels.
#[unsafe(no_mangle)]
pub extern "C" fn nxkit_resize(width: u32, height: u32) {
    if width > 0 && height > 0 {
        with_session(|s| s.resize(width, height));
    }
}
// SAFETY: points refers to count initialized C-compatible Touch values, or is
// null for an empty snapshot. Swift keeps the array alive throughout this call.
/// Passes the current normalized touch set to the game.
///
/// # Safety
/// `points` must refer to `count` initialized Touch values, or be null when count is zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nxkit_touches(points: *const Touch, count: usize) {
    if !main_thread() {
        return;
    }
    if count > 16 || (points.is_null() && count != 0) {
        with_session(|s| s.touches(&[]));
        return;
    }
    let points = if count == 0 {
        &[]
    } else {
        // SAFETY: non-null, bounded array according to the caller contract.
        unsafe { std::slice::from_raw_parts(points, count) }
    };
    with_session(|s| {
        s.touches(if super::valid_touches(points) {
            points
        } else {
            &[]
        })
    });
}
/// Returns the requested picture aspect, or zero when filling the drawable.
#[unsafe(no_mangle)]
pub extern "C" fn nxkit_picture_aspect() -> f64 {
    // Keep the existing ABI. The shared presenter fills the drawable.
    0.0
}
/// Requests game shutdown.
#[unsafe(no_mangle)]
pub extern "C" fn nxkit_stop() {
    // Keep the session and all retained resources alive until process exit.
    // This shared kernel cannot safely restart or unload detached guest threads.
    with_session(GameSession::stop);
}

/// A complete virtual-pad snapshot, never a delta. Sanitized by the input adapter.
/// Replaces the virtual player-one controller state.
#[unsafe(no_mangle)]
pub extern "C" fn nxkit_virtual_pad(buttons: u64, lx: f32, ly: f32, rx: f32, ry: f32) {
    with_session(|s| s.virtual_pad(buttons, lx, ly, rx, ry));
}
/// Reports whether a physical extended-profile controller is connected.
#[unsafe(no_mangle)]
pub extern "C" fn nxkit_has_controller() -> bool {
    if !main_thread() {
        return false;
    }
    SESSION.with_borrow(|s| s.as_ref().is_some_and(|s| s.has_controller()))
}
/// Reports whether the guest uses handheld mode.
#[unsafe(no_mangle)]
pub extern "C" fn nxkit_is_handheld() -> bool {
    if !main_thread() {
        return false;
    }
    SESSION.with_borrow(|s| s.as_ref().is_some_and(|s| s.is_handheld()))
}
