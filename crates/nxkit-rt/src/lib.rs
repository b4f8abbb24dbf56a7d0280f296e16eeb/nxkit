//! The nxkit runtime for the host: a dynamic library (`libnxkit_rt.dylib`) with a C ABI.
//! The host loads it with `dlopen` when a service is enabled. The crate also builds as a static library for a later iOS path.
//!
//! The host calls [`nxkit_init`] once with the services that nxkit runs, then asks [`nxkit_hle_find`] for a function
//! by its Switch mangled name. A name of a service that is not enabled gives NULL, so the host uses its own function.
//! With an empty mask, or before init, every lookup gives NULL and the host behaves as without this library.
//!
//! The C header is `include/nxkit_rt.h`.
//!
//! Rules for a guest-facing function that a service adds:
//! - It is `extern "C"` and takes an argument of fewer than 32 bits as `u32`, because Switch code does not extend it.
//! - It is never variadic, because Switch code passes variadic arguments in registers.
//! - A function that needs a large stack must switch to a host stack, because a guest stack can be 8 KB.
//!   No service needs this yet and none switches.
//!
//! The `kernel` service forwards svc calls to `nxkit-hos` ([`nxkit_svc`]). The kernel is created by
//! [`nxkit_kernel_init`] and the host calls it from the gate of every svc.
//!
//! This crate may use `unsafe`, with `nxb-sys` and the module `sys` of `nxkit-hos`. The `unsafe` blocks are as small as possible.

use std::ffi::{CStr, c_char, c_void};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

mod kernel;
mod offline;

pub use kernel::{
    NxkitKernelConfig, NxkitRoleRule, NxkitThreadEntry, nxkit_kernel_dump_stats, nxkit_kernel_init,
    nxkit_kernel_set_thread_name, nxkit_svc,
};

/// A service that nxkit can run. The bit of a service in the mask is its index in `nxtitle::Service::ALL`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Service {
    /// `fs`
    Fs,
    /// `input`
    Input,
    /// `audio`
    Audio,
    /// `video`
    Video,
    /// `system`
    System,
    /// `offline`
    Offline,
    /// `pacing`
    Pacing,
    /// `kernel`
    Kernel,
    /// `gpu`
    Gpu,
}

impl Service {
    /// Every service, in the order of `nxtitle::Service::ALL`.
    pub const ALL: [Service; 9] = [
        Service::Fs,
        Service::Input,
        Service::Audio,
        Service::Video,
        Service::System,
        Service::Offline,
        Service::Pacing,
        Service::Kernel,
        Service::Gpu,
    ];

    /// The bit of the service in the mask.
    pub const fn bit(self) -> u64 {
        1 << self as u32
    }

    /// The mask with every service.
    pub const fn all_bits() -> u64 {
        (1 << Service::ALL.len() as u32) - 1
    }
}

/// One function of the runtime, found by the exact bytes of its Switch mangled name.
#[derive(Clone, Copy, Debug)]
pub struct Entry {
    /// The Switch mangled name.
    pub name: &'static str,
    /// The service that the function belongs to.
    pub service: Service,
    /// The address of the function.
    pub addr: usize,
}

/// The functions of the runtime, built once. The addresses of functions are not constants, so it is not a `static` slice.
pub fn entries() -> &'static [Entry] {
    static TABLE: OnceLock<Vec<Entry>> = OnceLock::new();
    TABLE.get_or_init(offline::entries)
}

/// The mask of enabled services. Zero before init.
static SERVICES: AtomicU64 = AtomicU64::new(0);

/// Finds `name` in `table`. Only a service in `mask` is found.
pub fn find_in(table: &[Entry], mask: u64, name: &[u8]) -> Option<usize> {
    table
        .iter()
        .find(|e| e.service.bit() & mask != 0 && e.name.as_bytes() == name)
        .map(|e| e.addr)
}

/// Sets the enabled services. Bits that name no service are ignored.
/// A later call replaces the mask of an earlier call (the last call wins).
/// Safe from any thread.
#[unsafe(no_mangle)]
pub extern "C" fn nxkit_init(services: u64) {
    SERVICES.store(services & Service::all_bits(), Ordering::Release);
}

/// Returns the function for a Switch mangled name, or NULL.
/// NULL also means that the service of the name is not enabled or that [`nxkit_init`] was not called.
///
/// # Safety
///
/// `name` is NULL or points to a NUL-terminated string that stays valid during the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nxkit_hle_find(name: *const c_char) -> *const c_void {
    if name.is_null() {
        return std::ptr::null();
    }
    // SAFETY: `name` is not NULL and the caller promises a valid NUL-terminated string.
    let name = unsafe { CStr::from_ptr(name) };
    match find_in(entries(), SERVICES.load(Ordering::Acquire), name.to_bytes()) {
        Some(addr) => addr as *const c_void,
        None => std::ptr::null(),
    }
}

/// The names of the exported C symbols. The header has one declaration for each.
pub const EXPORTS: [&str; 6] = [
    "nxkit_init",
    "nxkit_hle_find",
    "nxkit_svc",
    "nxkit_kernel_init",
    "nxkit_kernel_set_thread_name",
    "nxkit_kernel_dump_stats",
];

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" fn probe(x: u32) -> u32 {
        x + 1
    }

    fn probe_addr() -> usize {
        probe as *const () as usize
    }

    fn table() -> [Entry; 2] {
        [
            Entry {
                name: "_ZN2nn3fs5ProbeEj",
                service: Service::Fs,
                addr: probe_addr(),
            },
            Entry {
                name: "_ZN2nn3net5ProbeEj",
                service: Service::Offline,
                addr: probe_addr() + 4,
            },
        ]
    }

    #[test]
    fn empty_mask_finds_nothing() {
        assert_eq!(find_in(&table(), 0, b"_ZN2nn3fs5ProbeEj"), None);
    }

    #[test]
    fn only_enabled_services_are_found() {
        let t = table();
        let fs = Service::Fs.bit();
        assert_eq!(find_in(&t, fs, b"_ZN2nn3fs5ProbeEj"), Some(probe_addr()));
        assert_eq!(find_in(&t, fs, b"_ZN2nn3net5ProbeEj"), None);
        assert_eq!(
            find_in(&t, Service::Offline.bit(), b"_ZN2nn3net5ProbeEj"),
            Some(probe_addr() + 4)
        );
    }

    #[test]
    fn the_name_matches_exactly() {
        let t = table();
        let all = Service::all_bits();
        assert_eq!(find_in(&t, all, b"_ZN2nn3fs5ProbeE"), None);
        assert_eq!(find_in(&t, all, b"_ZN2nn3fs5ProbeEjj"), None);
        assert_eq!(find_in(&t, all, b"_zn2nn3fs5probeej"), None);
    }

    #[test]
    fn the_first_entry_of_a_name_wins() {
        let mut t = table();
        t[1].name = t[0].name;
        t[1].service = Service::Fs;
        assert_eq!(
            find_in(&t, Service::Fs.bit(), t[0].name.as_bytes()),
            Some(probe_addr())
        );
    }

    #[test]
    fn a_null_name_gives_null() {
        // SAFETY: a NULL name is allowed.
        assert!(unsafe { nxkit_hle_find(std::ptr::null()) }.is_null());
    }
}
