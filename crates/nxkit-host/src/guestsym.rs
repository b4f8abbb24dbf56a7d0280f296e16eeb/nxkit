//! The lookup of the game's own symbols by their Switch mangled name.
//!
//! Several service crates call a function that the game's SDK defines, for example `nn::os::SignalSystemEvent`. Each
//! crate has its own seam for the lookup, and all of them end here. The answer is the address of the SDK's own
//! definition, taken from the export lists of the modules. It is never the address of a take-over export, because
//! the export lists hold the symbols of the images, not the import slots that the loader filled.

use std::collections::HashMap;
use std::sync::OnceLock;

static SYMBOLS: OnceLock<HashMap<String, u64>> = OnceLock::new();

/// Sets the symbols of the loaded modules. The first call wins.
pub fn set(symbols: HashMap<String, u64>) -> bool {
    SYMBOLS.set(symbols).is_ok()
}

/// The address of the game's own definition of `name`.
pub fn find(name: &str) -> Option<u64> {
    SYMBOLS.get()?.get(name).copied()
}

fn find_usize(name: &str) -> Option<usize> {
    usize::try_from(find(name)?).ok()
}

fn find_or_zero(name: &str) -> usize {
    find_usize(name).unwrap_or(0)
}

/// Sets this lookup in the kernel and in fs. The system, audio and video seams get it in `wire`. The symbols
/// themselves arrive with [`set`] after the load, and the crates ask only at run time.
pub fn register() {
    let _ = nxkit_hos::set_guest_sym(find_usize);
    nxkit_svc_fs::set_guest_sym(find);
}

/// The lookup in the shape of the audio `Host.guest_sym`: 0 for a symbol that is not there.
pub(crate) fn audio_lookup(name: &str) -> usize {
    find_or_zero(name)
}

/// The lookup in the shape of the system `Seams.guest_sym`.
pub(crate) fn system_lookup(name: &str) -> Option<u64> {
    find(name)
}

/// The lookup in the shape of the video `Host::resolve`.
pub(crate) fn video_lookup(name: &str) -> Option<usize> {
    find_usize(name)
}
