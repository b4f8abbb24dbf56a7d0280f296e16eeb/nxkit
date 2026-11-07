//! The lookup of the SDK's own functions by mangled name (nxnative `guest_sym`).
//!
//! Some take-overs call the function they replace, or call other SDK functions, because the SDK's `nn::os` runs
//! unchanged on the kernel (`StandardAllocator::Allocate`, `getenv`, the host-made guest threads and the system
//! event binding). The host knows the modules, so it sets the lookup once with [`set_guest_sym`] before the game
//! starts.
//!
//! Contract of the lookup: it answers with the entry of the SDK module's own definition, never with the export that
//! the host wrote into the import slots, and the function has the signature that its mangled name stands for.
//! The `sys` module calls through these addresses on that promise.

use crate::sys::GuestFn;
use std::sync::OnceLock;

/// The lookup: the entry address of the SDK function with this mangled name, or `None`.
pub type GuestSymFn = fn(&str) -> Option<usize>;

static LOOKUP: OnceLock<GuestSymFn> = OnceLock::new();

/// Sets the lookup. Only the first call wins. Returns false for a later call.
pub fn set_guest_sym(lookup: GuestSymFn) -> bool {
    LOOKUP.set(lookup).is_ok()
}

/// The entry address of the SDK function `name`. `None` when no lookup is set, when the SDK has no such function,
/// or when the host answers with address 0.
pub fn guest_sym(name: &str) -> Option<usize> {
    LOOKUP
        .get()
        .and_then(|lookup| lookup(name))
        .filter(|addr| *addr != 0)
}

/// The SDK function `name`, or an error that names the missing lookup or symbol.
pub(crate) fn require(name: &str) -> Result<GuestFn, String> {
    match guest_sym(name) {
        Some(addr) => Ok(GuestFn::new(addr)),
        None if LOOKUP.get().is_none() => Err(format!(
            "the guest symbol lookup is not set (set_guest_sym), so {name} cannot be found"
        )),
        None => Err(format!("the SDK has no function {name}")),
    }
}
