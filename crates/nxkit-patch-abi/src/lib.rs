//! The patch module contract of the nxkit host (W38).
//!
//! An AI agent or a person tests a change of the host's behaviour in a running game. The host cannot replace its own
//! Rust code: Rust has no stable ABI, and the state lives in objects and statics. Instead the host has a few fixed
//! hook points and this narrow C ABI. A patch module is a small `cdylib` that exports two functions. The host loads
//! it with `dlopen` through the control socket (`patch load PATH`) and can unload and replace it while the game runs.
//!
//! The hook points are in the encoder of `nxkit-gpu`:
//!
//! - frame begin and frame end ([`FrameBegin`], [`FrameEnd`]),
//! - before each render pass ([`PassInfo`] in, [`PassAnswer`] out),
//! - before each draw ([`DrawInfo`] in, [`DrawAnswer`] out).
//!
//! The host also gives the patch a log function ([`HostApi::log`]).
//!
//! A patch changes behaviour only through the answers of these hooks. The answers are plain data. The default answer
//! (all zero) changes nothing. The ABI has an exact version ([`ABI_VERSION`]): the host refuses a patch of another
//! version. Any change of a struct in this crate changes the version. The layout tests in `types.rs` fail when a
//! struct changes without it.
//!
//! The crate has no `unsafe` code. A patch needs `unsafe` only for the two exported symbols, and
//! [`export_patch!`] writes them.
//!
//! [`registry`] is the host side: the list of loaded patches and the dispatch of the hooks. Its cost without a loaded
//! patch is one relaxed atomic load per hook point.

mod answer;
pub mod registry;
mod table;
mod types;

pub use answer::*;
pub use table::*;
pub use types::*;

/// The version of the ABI. A patch exports it in `nxkit_patch_abi_version`. The host loads only an exact match.
pub const ABI_VERSION: u32 = 1;

/// The name of the symbol that returns [`ABI_VERSION`] as the patch was built.
pub const SYMBOL_VERSION: &str = "nxkit_patch_abi_version";

/// The name of the symbol that the host calls once after it checked the version.
pub const SYMBOL_INIT: &str = "nxkit_patch_init";

/// The type of the exported `nxkit_patch_abi_version`.
pub type AbiVersionFn = extern "C" fn() -> u32;

/// The type of the exported `nxkit_patch_init`. The patch fills `table` and returns 0, or returns another number to
/// refuse the load.
pub type InitFn = extern "C" fn(host: &HostApi, table: &mut PatchTable) -> u32;

/// Exports the two symbols of a patch module. `$init` is a function `fn(&HostApi, &mut PatchTable) -> bool`. It
/// sets the name and the hooks in the table and returns true. Every hook is an `extern "C" fn`.
///
/// ```ignore
/// extern "C" fn draw(info: &DrawInfo, answer: &mut DrawAnswer) { /* ... */ }
///
/// nxkit_patch_abi::export_patch!(|host, table| {
///     table.set_name("my-patch");
///     table.draw = Some(draw);
///     host.log("loaded");
///     true
/// });
/// ```
#[macro_export]
macro_rules! export_patch {
    ($init:expr) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn nxkit_patch_abi_version() -> u32 {
            $crate::ABI_VERSION
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn nxkit_patch_init(
            host: &$crate::HostApi,
            table: &mut $crate::PatchTable,
        ) -> u32 {
            let init: fn(&$crate::HostApi, &mut $crate::PatchTable) -> bool = $init;
            if init(host, table) { 0 } else { 1 }
        }
    };
}
