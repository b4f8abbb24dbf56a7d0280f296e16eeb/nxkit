//! The SDK take-overs of the kernel service behind a C ABI: the lookup of the SDK's own functions, the facts of the
//! game that the take-overs need, and the entropy mode.
//!
//! The functions themselves are `nxkit_hos::exports()`, and the host finds them with `nxkit_hle_find`. A name in
//! `nxkit_hos::forced_names()` must win over the SDK's own definition, as `HLE_FORCE` does in nxnative.

use nxkit_hos::{OsConfig, entropy, init_os, set_guest_sym};
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::OnceLock;

/// Finds a function of the game's own code by its Switch mangled name. It returns the SDK's definition and never
/// the nxkit export of the same name. NULL means the symbol does not exist.
pub type NxkitGuestSym = extern "C" fn(name: *const c_char) -> *const c_void;

static GUEST_SYM: OnceLock<NxkitGuestSym> = OnceLock::new();

fn lookup(name: &str) -> Option<usize> {
    let lookup = GUEST_SYM.get()?;
    let c = CString::new(name).ok()?;
    Some(lookup(c.as_ptr()) as usize)
}

/// Sets the lookup of the SDK's own functions. Call it once before the game starts. Returns 0 on success and 1
/// when a lookup is set already (the call changes nothing).
#[unsafe(no_mangle)]
pub extern "C" fn nxkit_kernel_set_guest_sym(lookup_fn: NxkitGuestSym) -> i32 {
    if GUEST_SYM.set(lookup_fn).is_err() || !set_guest_sym(lookup) {
        return 1;
    }
    0
}

/// The facts of the game for the take-overs.
#[repr(C)]
pub struct NxkitOsConfig {
    /// The names of the environment that `getenv` answers, `env_count` NUL-terminated strings.
    pub env_names: *const *const c_char,
    /// The values for the names, `env_count` NUL-terminated strings.
    pub env_values: *const *const c_char,
    /// The number of environment entries.
    pub env_count: usize,
    /// Non-zero aligns `StandardAllocator::Allocate` of 64 KB or more to 64 KB (profile key `alloc_big_align`).
    pub alloc_big_align: u32,
}

/// Sets the facts of the game. Call it once before the game starts. Returns 0 on success, 1 when they are set
/// already and -1 for a bad configuration.
///
/// # Safety
///
/// `config` points to a valid [`NxkitOsConfig`]. Its arrays have `env_count` NUL-terminated strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nxkit_kernel_os_init(config: *const NxkitOsConfig) -> i32 {
    if config.is_null() {
        return -1;
    }
    // SAFETY: the caller promises a valid configuration.
    let c = unsafe { &*config };
    let mut game_env = Vec::new();
    if c.env_count != 0 {
        if c.env_names.is_null() || c.env_values.is_null() {
            return -1;
        }
        // SAFETY: the caller promises `env_count` strings in each array.
        let (names, values) = unsafe {
            (
                std::slice::from_raw_parts(c.env_names, c.env_count),
                std::slice::from_raw_parts(c.env_values, c.env_count),
            )
        };
        for (n, v) in names.iter().zip(values) {
            if n.is_null() || v.is_null() {
                return -1;
            }
            // SAFETY: the caller promises NUL-terminated strings.
            let (n, v) = unsafe { (CStr::from_ptr(*n), CStr::from_ptr(*v)) };
            game_env.push((
                n.to_string_lossy().into_owned(),
                v.to_string_lossy().into_owned(),
            ));
        }
    }
    let config = OsConfig {
        game_env,
        alloc_big_align: c.alloc_big_align != 0,
    };
    match init_os(config) {
        Ok(()) => 0,
        Err(why) => {
            eprintln!("nxkit-rt: {why}");
            1
        }
    }
}

/// Switches the random bytes of `getentropy`, the csrng export and `GetInfo` type 11 to a fixed stream (non-zero)
/// or back to random bytes (zero). This is the `DM_DETCLOCK` mode of nxnative. Call it before the game starts.
#[unsafe(no_mangle)]
pub extern "C" fn nxkit_kernel_set_deterministic(on: u32) {
    entropy::set_deterministic(on != 0);
}

/// The number of SDK functions that the host must resolve over the SDK's own definition, and the name of each.
/// Writes the names as pointers to static NUL-terminated strings into `out` (at most `capacity`) and returns the
/// total count.
///
/// # Safety
///
/// `out` is NULL or points to `capacity` writable pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nxkit_kernel_forced_names(
    out: *mut *const c_char,
    capacity: usize,
) -> usize {
    let names = forced_c_names();
    if !out.is_null() {
        for (i, n) in names.iter().take(capacity).enumerate() {
            // SAFETY: `out` has `capacity` entries and `i < capacity`.
            unsafe { *out.add(i) = n.as_ptr() };
        }
    }
    names.len()
}

fn forced_c_names() -> &'static [CString] {
    static NAMES: OnceLock<Vec<CString>> = OnceLock::new();
    NAMES.get_or_init(|| {
        nxkit_hos::forced_names()
            .iter()
            .filter_map(|n| CString::new(*n).ok())
            .collect()
    })
}
