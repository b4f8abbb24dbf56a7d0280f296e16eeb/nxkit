//! UIKit presentation and adoption of validated dyld-owned guest mappings.

use super::{Dylib, Reservation};
use crate::{Game, HostError, Result, image, layout, macho, sys, tail};
use std::ptr::NonNull;

/// Verifies dyld mappings before adopting the process-lifetime reservation.
pub(crate) fn map(game: &Game, dylib: &Dylib) -> Result<(Game, layout::Layout, Reservation)> {
    let mapped = dylib.image()?;
    let commands = mapped.commands()?;
    image::validate(&commands)?;
    let segments = macho::segments(&commands)?;
    for module in &game.modules {
        macho::verify(module, &segments)?;
    }
    let plan = tail::plan(game, &segments, mapped.slide)?;
    let layout = plan.layout;
    if !layout.fits_address_space() {
        return Err(HostError::Memory(
            "dyld placed the game outside the 39-bit guest address space".into(),
        ));
    }
    // Leave unused dyld padding untouched. Only the anonymous tail is adopted
    // and checked below; the GPU buffer excludes the padding.
    // SAFETY: the mapped commands and actual slide passed the tail contract.
    // The successful session retains the dylib for the rest of the process;
    // failed placement closes it only before any guest view is registered.
    let res =
        unsafe { sys::adopt_dyld_tail(layout.all.start, plan.reserved.start, layout.all.end())? };
    Ok((plan.game, layout, res))
}

/// Creates an iOS presenter while the caller retains the UIKit layer.
pub(crate) fn presenter(
    context: &nxkit_metal::Context,
    layer: &objc2::rc::Retained<objc2_quartz_core::CAMetalLayer>,
) -> std::result::Result<nxkit_metal::Presenter, nxkit_metal::MetalError> {
    // SAFETY: The caller holds the layer; the presenter retains it before this
    // reference can expire. The caller is the main-thread-only session factory.
    unsafe { nxkit_metal::Presenter::from_raw_layer(context, NonNull::from(&**layer).cast()) }
}
