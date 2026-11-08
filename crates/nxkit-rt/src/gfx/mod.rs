//! The SDK take-overs of the graphics service (R59).
//!
//! nxnative replaces the graphics glue of the SDK with host functions: the `nv::` driver layer beneath NVN, the
//! `nn::vi` display and layer calls, and the native images of `nn::gs`. NVN itself is [`crate::nvn`], so the SDK
//! code of these layers has nothing to do and never opens the `nvdrv` or `vi` service. The functions are exports of
//! the `gpu` service (`Service::Gpu`) and are forced take-overs, as `HLE_FORCE` of nxnative.
//!
//! One module follows each source file of nxnative `dm/gfx/`. The module docs name the file and the functions. The
//! calling convention is the one of `nxkit_abi::export`: narrow integers are `u32`, pointers are `u64` guest
//! addresses.

pub(crate) mod nv;

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

use nxkit_abi::export::Export;
use nxkit_abi::guest::Fault;

/// Builds the export list of a module from `"mangled name" => function` rows.
macro_rules! table {
    ($($name:literal => $f:expr,)*) => {
        /// The exports of the module.
        pub(crate) fn exports() -> Vec<nxkit_abi::export::Export> {
            vec![$(nxkit_abi::export::Export::new($name, $f as *const () as usize)),*]
        }
    };
}
pub(crate) use table;

/// All forced take-overs of the graphics service.
pub(crate) fn exports() -> Vec<Export> {
    let mut v = Vec::new();
    v.extend(nv::exports());
    v
}

/// Prints a line of the log of the take-overs.
pub(crate) fn log(msg: impl fmt::Display) {
    eprintln!("[gfx] {msg}");
}

/// Logs a line once per flag. The message is built only the first time.
pub(crate) fn log_once_with(flag: &AtomicBool, msg: impl FnOnce() -> String) {
    if !flag.swap(true, Ordering::Relaxed) {
        log(msg());
    }
}

/// Ends the game and says which function did it. The process aborts, as an NVN failure does (R15).
pub(crate) fn fatal(name: &str, why: impl fmt::Display) -> ! {
    eprintln!("[RED] {name}: {why}");
    std::process::abort()
}

/// The result of a guest memory access. A fault ends the game, as the console stops on a bad pointer of the game.
pub(crate) fn checked<T>(name: &str, r: Result<T, Fault>) -> T {
    match r {
        Ok(v) => v,
        Err(e) => fatal(name, e),
    }
}

/// True when the game is of an SDK up to 10. Its NVN API is 53, and the host has no other fact of the SDK version.
/// Without a GPU context the answer is no.
pub(crate) fn old_sdk() -> bool {
    crate::nvn::ctx::settings_if_installed().is_some_and(|s| s.api_major <= 53)
}
