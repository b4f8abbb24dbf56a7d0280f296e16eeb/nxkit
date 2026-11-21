//! The Metal performance HUD over the game (`NXKIT_METAL_HUD`, a launch option).

use objc2::rc::Retained;
use objc2_foundation::{NSDictionary, NSString};
use objc2_quartz_core::CAMetalLayer;

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
