//! The Metal performance HUD over the game (`NXKIT_METAL_HUD`, a launch option).

use objc2::rc::Retained;
use objc2_foundation::{NSDictionary, NSString};
use objc2_quartz_core::CAMetalLayer;

/// Turns Metal's HUD on for the process when `on`. The layer property of [`show`] alone shows nothing: Metal draws the
/// HUD only in a process that starts its first device with `MTL_HUD_ENABLED=1` in the environment. The host sets it
/// here, before any Metal device exists and before it starts a thread of its own, so no other thread reads the
/// environment at the same time.
pub(crate) fn enable(on: bool) {
    if on {
        // SAFETY: called on the main thread before the host starts any thread (the log pipe, the crash handler, Metal).
        unsafe { std::env::set_var("MTL_HUD_ENABLED", "1") };
    }
}

/// Lets Metal make GPU trace captures when `on`: Metal checks `MTL_CAPTURE_ENABLED` when its first device starts. The
/// same rules as [`enable`] hold.
pub(crate) fn enable_capture(on: bool) {
    if on {
        // SAFETY: called on the main thread before the host starts any thread.
        unsafe { std::env::set_var("MTL_CAPTURE_ENABLED", "1") };
    }
}

/// Shows Metal's HUD on `layer` when `on`, through `developerHUDProperties` (`mode` = `default`).
pub(super) fn show(layer: &CAMetalLayer, on: bool) {
    if !on {
        return;
    }
    let (key, mode) = (NSString::from_str("mode"), NSString::from_str("default"));
    let hud = NSDictionary::from_slices(&[&*key], &[&*mode]);
    // SAFETY: a dictionary of strings is a dictionary of objects, and the HUD takes string keys and values.
    unsafe {
        let hud: Retained<NSDictionary> = Retained::cast_unchecked(hud);
        layer.setDeveloperHUDProperties(Some(&hud));
    }
}
