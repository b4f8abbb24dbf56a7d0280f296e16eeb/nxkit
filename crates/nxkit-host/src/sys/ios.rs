//! Raw UIKit C ABI and Objective-C pointer boundary. Session policy stays in `crate::ios`.
use crate::{HostError, Result, ios};
use nxkit_metal::{Context, Presenter};
use objc2::{MainThreadMarker, rc::Retained};
use objc2_quartz_core::CAMetalLayer;
use std::{
    ffi::{CStr, c_char, c_void},
    path::Path,
    ptr::NonNull,
};

pub(crate) fn presenter(context: &Context, layer: &CAMetalLayer) -> Result<Presenter> {
    // SAFETY: a borrowed CAMetalLayer is live; Presenter retains it before returning.
    unsafe { Presenter::from_raw_layer(context, NonNull::from(layer).cast()) }
        .map_err(|e| HostError::App(format!("Metal: {e}")))
}

#[unsafe(no_mangle)]
unsafe extern "C" fn nxkit_start(
    layer: *mut c_void,
    bundle: *const c_char,
    home: *const c_char,
    maximum_fps: u32,
    error: *mut c_char,
    capacity: usize,
) -> bool {
    let result = (|| {
        if MainThreadMarker::new().is_none()
            || layer.is_null()
            || bundle.is_null()
            || home.is_null()
        {
            return Err("Invalid UIKit session arguments".to_string());
        }
        // SAFETY: the Swift caller passes live NUL-terminated strings during this call.
        let bundle = unsafe { CStr::from_ptr(bundle) }
            .to_str()
            .map_err(|e| e.to_string())?;
        // SAFETY: same borrowed string contract for the process sandbox home.
        let home = unsafe { CStr::from_ptr(home) }
            .to_str()
            .map_err(|e| e.to_string())?;
        // SAFETY: UIKit owns this live CAMetalLayer; retain it before storing it.
        let layer = unsafe { Retained::retain(layer.cast::<CAMetalLayer>()) }
            .ok_or("Missing Metal layer")?;
        ios::start(layer, Path::new(bundle), Path::new(home), maximum_fps)
            .map_err(|e| e.to_string())?;
        Ok::<(), String>(())
    })();
    if !error.is_null() && capacity > 0 {
        let message = result.as_ref().err().map_or("", String::as_str);
        let mut end = message.len().min(capacity - 1);
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        // SAFETY: caller provides capacity writable bytes; never copy past that bound.
        unsafe {
            std::ptr::copy_nonoverlapping(message.as_ptr(), error.cast(), end);
            *error.add(end) = 0;
        }
    }
    result.is_ok()
}
#[unsafe(no_mangle)]
extern "C" fn nxkit_set_active(active: bool) {
    ios::set_active(active);
}
