//! Functions that a service gives the game in place of an SDK function.
//!
//! The loader writes the address of such a function into the import table of the game's module (R14). A call of the
//! game is then a plain branch into Rust.
//!
//! Rules for a guest-facing function:
//! - It is `extern "C"` and takes an argument of fewer than 32 bits as `u32`, because Switch code does not extend it.
//! - It is never variadic, because Switch code passes variadic arguments in registers.
//! - A pointer argument is a `u64` guest address and is read through [`crate::guest`].
//! - A function that needs a large stack must switch to a host stack, because a guest stack can be 8 KB.

/// One exported function, found by the exact bytes of its Switch mangled name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Export {
    /// The Switch mangled name, for example `_ZN2nn3hid13GetNpadStatesEPNS0_16NpadFullKeyStateEiRKj`.
    pub name: &'static str,
    /// The address of the `extern "C"` function.
    pub addr: usize,
}

impl Export {
    /// An export of `addr` under `name`.
    pub const fn new(name: &'static str, addr: usize) -> Export {
        Export { name, addr }
    }
}
