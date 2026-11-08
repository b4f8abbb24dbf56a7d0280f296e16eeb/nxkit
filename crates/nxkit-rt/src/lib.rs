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
//!   The runtime does not switch. A service lists such a function in its `big_stack_exports()`, and the host finds it
//!   with [`nxkit_hle_find_big`] and runs it on its big stack. [`nxkit_hle_find`] never returns it.
//!   The audio functions that start and stop Core Audio need this. No other service but `fs` needs it.
//!
//! The `kernel` service forwards svc calls to `nxkit-hos` ([`nxkit_svc`]). The kernel is created by
//! [`nxkit_kernel_init`] and the host calls it from the gate of every svc.
//!
//! This crate may use `unsafe`, with `nxb-sys` and the module `sys` of `nxkit-hos`. The `unsafe` blocks are as small as possible.

use std::ffi::{CStr, c_char, c_void};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// Whether `NXKIT_DIAG` names the `cmd` category (or `all`): the per-command-buffer lines are written only then.
pub fn diag_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("NXKIT_DIAG")
            .map(|v| v.split(',').any(|c| c == "cmd" || c == "all"))
            .unwrap_or(false)
    })
}

/// `eprintln!` behind the `cmd` category of `NXKIT_DIAG`.
#[macro_export]
macro_rules! diag {
    ($($t:tt)*) => { if $crate::diag_on() { eprintln!($($t)*); } };
}

mod audio;
mod gfx;
mod guestos;
mod kernel;
pub mod nvn;

pub use audio::{NxkitAudioConfig, NxkitAudioHost, nxkit_audio_init};
pub use guestos::{
    NxkitGuestSym, NxkitOsConfig, nxkit_kernel_forced_names, nxkit_kernel_os_init,
    nxkit_kernel_set_deterministic, nxkit_kernel_set_guest_sym,
};
pub use kernel::{
    attach_main_thread, kernel, NxkitKernelConfig, NxkitMemoryConfig, NxkitModule, NxkitRoleRule, NxkitThreadEntry,
    nxkit_kernel_dump_stats, nxkit_kernel_init, nxkit_kernel_set_thread_name,
    nxkit_kernel_thread_tls, nxkit_svc,
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
    /// True when the function needs the big host stack (`HLE_FORCE_BIG` in nxnative). The host finds it with
    /// [`nxkit_hle_find_big`] and runs it on that stack.
    pub big_stack: bool,
}

/// The functions of the runtime, built once. The addresses of functions are not constants, so it is not a `static` slice.
/// The SDK take-overs of the kernel service come after the offline entries.
pub fn entries() -> &'static [Entry] {
    static TABLE: OnceLock<Vec<Entry>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = Vec::new();
        table.extend(mapped(nxkit_svc_fs::exports(), Service::Fs, false));
        table.extend(mapped(nxkit_svc_fs::big_stack_exports(), Service::Fs, true));
        table.extend(mapped(nxkit_svc_offline::exports(), Service::Offline, false));
        table.extend(mapped(nxkit_svc_input::exports(), Service::Input, false));
        table.extend(mapped(nxkit_svc_video::exports(), Service::Video, false));
        table.extend(mapped(nxkit_svc_audio::exports(), Service::Audio, false));
        table.extend(mapped(nxkit_svc_audio::big_stack_exports(), Service::Audio, true));
        table.extend(mapped(nxkit_svc_pacing::exports(), Service::Pacing, false));
        table.extend(mapped(nxkit_svc_system::exports(), Service::System, false));
        table.extend(mapped(nxkit_svc_system::big_stack_exports(), Service::System, true));
        table.push(Entry {
            name: nvn::BOOTSTRAP_LOADER_NAME,
            service: Service::Gpu,
            addr: nvn::nvn_bootstrap_loader as extern "C" fn(u64) -> u64 as usize,
            big_stack: false,
        });
        table.extend(mapped(gfx::exports(), Service::Gpu, false));
        table.extend(mapped(nxkit_hos::exports(), Service::Kernel, false));
        table
    })
}

/// The exports of one service crate as entries of `service`.
fn mapped(exports: Vec<nxkit_abi::export::Export>, service: Service, big_stack: bool) -> impl Iterator<Item = Entry> {
    exports.into_iter().map(move |e| Entry {
        name: e.name,
        service,
        addr: e.addr,
        big_stack,
    })
}

/// The mask of enabled services. Zero before init.
static SERVICES: AtomicU64 = AtomicU64::new(0);

/// Finds `name` in `table`. Only a service in `mask` is found. Functions that need the big stack are not found.
pub fn find_in(table: &[Entry], mask: u64, name: &[u8]) -> Option<usize> {
    find_kind(table, mask, name, false)
}

/// Finds `name` among the functions that need the big stack. Only a service in `mask` is found.
pub fn find_big_in(table: &[Entry], mask: u64, name: &[u8]) -> Option<usize> {
    find_kind(table, mask, name, true)
}

fn find_kind(table: &[Entry], mask: u64, name: &[u8], big_stack: bool) -> Option<usize> {
    table
        .iter()
        .find(|e| e.big_stack == big_stack && e.service.bit() & mask != 0 && e.name.as_bytes() == name)
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

/// Returns the function for a Switch mangled name among those that need the big host stack, or NULL.
/// The host runs such a function on its big stack (`HLE_FORCE_BIG`). NULL has the meaning of [`nxkit_hle_find`].
///
/// # Safety
///
/// `name` is NULL or points to a NUL-terminated string that stays valid during the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nxkit_hle_find_big(name: *const c_char) -> *const c_void {
    if name.is_null() {
        return std::ptr::null();
    }
    // SAFETY: `name` is not NULL and the caller promises a valid NUL-terminated string.
    let name = unsafe { CStr::from_ptr(name) };
    match find_big_in(entries(), SERVICES.load(Ordering::Acquire), name.to_bytes()) {
        Some(addr) => addr as *const c_void,
        None => std::ptr::null(),
    }
}

/// The names of the exported C symbols. The header has one declaration for each.
pub const EXPORTS: [&str; 13] = [
    "nxkit_init",
    "nxkit_hle_find",
    "nxkit_hle_find_big",
    "nxkit_audio_init",
    "nxkit_svc",
    "nxkit_kernel_init",
    "nxkit_kernel_set_thread_name",
    "nxkit_kernel_dump_stats",
    "nxkit_kernel_thread_tls",
    "nxkit_kernel_set_guest_sym",
    "nxkit_kernel_os_init",
    "nxkit_kernel_set_deterministic",
    "nxkit_kernel_forced_names",
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
                big_stack: false,
            },
            Entry {
                name: "_ZN2nn3net5ProbeEj",
                service: Service::Offline,
                addr: probe_addr() + 4,
                big_stack: false,
            },
        ]
    }

    #[test]
    fn the_pacing_exports_are_in_the_table_under_the_pacing_service() {
        let name = nxkit_svc_pacing::VSYNC_EVENT;
        assert!(
            entries()
                .iter()
                .any(|e| e.name == name && e.service == Service::Pacing)
        );
        assert_eq!(
            find_in(entries(), Service::Offline.bit(), name.as_bytes()),
            None
        );
        assert!(find_in(entries(), Service::Pacing.bit(), name.as_bytes()).is_some());
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
    fn a_big_stack_function_is_found_only_as_one() {
        let mut t = table();
        t[0].big_stack = true;
        let fs = Service::Fs.bit();
        assert_eq!(find_in(&t, fs, b"_ZN2nn3fs5ProbeEj"), None);
        assert_eq!(find_big_in(&t, fs, b"_ZN2nn3fs5ProbeEj"), Some(probe_addr()));
        assert_eq!(find_big_in(&t, 0, b"_ZN2nn3fs5ProbeEj"), None);
    }

    #[test]
    fn the_fs_functions_of_nxnative_need_the_big_stack() {
        let all = Service::all_bits();
        let name = b"_ZN2nn2fs8OpenFileEPNS0_10FileHandleEPKci";
        assert_eq!(find_in(entries(), all, name), None);
        assert!(find_big_in(entries(), all, name).is_some());
    }

    #[test]
    fn a_null_name_gives_null() {
        // SAFETY: a NULL name is allowed.
        assert!(unsafe { nxkit_hle_find(std::ptr::null()) }.is_null());
    }
}
