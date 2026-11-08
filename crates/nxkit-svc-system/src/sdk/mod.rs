//! The SDK-level take-overs of the system crate (R38a).
//!
//! nxnative replaces `nn::*` functions of the game SDK with host functions. This crate does the same for the system
//! services, as `Export`s with `extern "C"` functions. An export wins over the SDK function, so the SDK code never
//! runs and never opens the IPC session. The IPC services of this crate answer the SDK code that no export replaces.
//! Every export of this crate is a forced take-over (`HLE_FORCE` of nxnative): it applies even when the module of the
//! game defines the symbol itself.
//!
//! One module follows each source file of nxnative `dm/host/`. The module docs name the file and the functions.
//! The calling convention is the one of `nxkit_abi::export`: narrow integers are `u32`, pointers are `u64` guest
//! addresses, and the functions read and write guest memory through `nxkit_abi::guest`.

use nxkit_abi::export::Export;
use nxkit_abi::guest;
use std::sync::atomic::{AtomicBool, Ordering};

/// Builds the export list of a module from `"mangled name" => function` rows.
macro_rules! table {
    ($($name:literal => $f:path,)*) => {
        /// The exports of the module.
        pub(super) fn exports() -> Vec<nxkit_abi::export::Export> {
            vec![$(nxkit_abi::export::Export::new($name, $f as *const () as usize)),*]
        }
    };
}

mod account;
mod aoc;
mod chat;
mod codec;
mod diag;
mod font;
mod friends;
mod init;
mod jit;
mod lifecycle;
mod mii;
mod misc;
mod net;
mod nfp;
mod ngc;
mod nvdrv;
mod oe;
mod oe_state;
mod offline;
mod ro;
mod sdk16;
mod settings;
mod sf;
mod shop;
mod socket;
mod stpl;
mod swkbd;
mod time;
mod time_detail;
mod web;

pub use lifecycle::quit;
pub(crate) use sdk16::mark_start;

/// All forced take-overs of the crate, without the ones that need a bigger guest stack.
pub(crate) fn all() -> Vec<Export> {
    let mut v = Vec::new();
    v.extend(init::exports());
    v.extend(oe::exports());
    v.extend(oe_state::exports());
    v.extend(sdk16::exports());
    v.extend(settings::exports());
    v.extend(diag::exports());
    v.extend(nfp::exports());
    v.extend(font::exports());
    v.extend(account::exports());
    v.extend(time::exports());
    v.extend(friends::exports());
    v.extend(mii::exports());
    v.extend(swkbd::exports());
    v.extend(web::exports());
    v.extend(misc::exports());
    v.extend(aoc::exports());
    v.extend(shop::exports());
    v.extend(ngc::exports());
    v.extend(stpl::exports());
    v.extend(chat::exports());
    v.extend(net::exports());
    v.extend(ro::exports());
    v.extend(sf::exports());
    v.extend(nvdrv::exports());
    v.extend(codec::exports());
    v.extend(jit::exports());
    v.extend(offline::exports());
    v.extend(time_detail::exports());
    v
}

/// The take-overs that nxnative registers with `HLE_FORCE_BIG`: the host runs them on a bigger stack.
pub(crate) fn big_stack() -> Vec<Export> {
    aoc::big_stack()
}

pub(super) use crate::util::RES_FAIL;

/// Returns success. Serves every export that ignores its arguments and returns a `Result` of 0.
pub(super) extern "C" fn ok() -> u32 {
    0
}

/// Returns the failure result of nxnative.
pub(super) extern "C" fn fail() -> u32 {
    RES_FAIL
}

/// Does nothing.
pub(super) extern "C" fn nothing() {}

/// Returns 0 as a `bool`, a count or a handle.
pub(super) extern "C" fn zero() -> u32 {
    0
}

/// Returns 0 as a 64-bit value or a null pointer.
pub(super) extern "C" fn zero64() -> u64 {
    0
}

/// Returns 1 as a `bool`.
pub(super) extern "C" fn one() -> u32 {
    1
}

/// Returns -1, the failure of the BSD calls.
pub(super) extern "C" fn neg1() -> i32 {
    -1
}

/// Prints the message and ends the process. A guest memory fault in an export is a bug of the game that the
/// console would stop on too.
pub(super) fn fatal(msg: &str) -> ! {
    eprintln!("nxkit-svc-system: {msg}");
    std::process::exit(1)
}

/// Prints a line of the log of the take-overs.
pub(super) fn log(msg: &str) {
    eprintln!("nxkit-svc-system: {msg}");
}

/// Logs a fixed line once per flag. A call after the first costs one atomic swap.
pub(super) fn log_once(flag: &AtomicBool, msg: &str) {
    if !flag.swap(true, Ordering::Relaxed) {
        log(msg);
    }
}

/// Logs a line once per flag. The message is built only the first time, so a call after that makes no allocation.
pub(super) fn log_once_with(flag: &AtomicBool, msg: impl FnOnce() -> String) {
    if !flag.swap(true, Ordering::Relaxed) {
        log(&msg());
    }
}

fn check<T>(r: Result<T, guest::Fault>) -> T {
    match r {
        Ok(v) => v,
        Err(e) => fatal(&format!("access to guest memory failed: {e:?}")),
    }
}

/// Writes a `u8` to `addr`. A null address is skipped, as nxnative skips a null output pointer.
pub(super) fn put_u8(addr: u64, v: u8) {
    if addr != 0 {
        check(guest::write_u8(addr, v));
    }
}

/// Writes a `u16` to `addr`. A null address is skipped.
pub(super) fn put_u16(addr: u64, v: u16) {
    if addr != 0 {
        check(guest::write_u16(addr, v));
    }
}

/// Writes a `u32` to `addr`. A null address is skipped.
pub(super) fn put_u32(addr: u64, v: u32) {
    if addr != 0 {
        check(guest::write_u32(addr, v));
    }
}

/// Writes an `i32` to `addr`. A null address is skipped.
pub(super) fn put_i32(addr: u64, v: i32) {
    if addr != 0 {
        check(guest::write_i32(addr, v));
    }
}

/// Writes a `u64` to `addr`. A null address is skipped.
pub(super) fn put_u64(addr: u64, v: u64) {
    if addr != 0 {
        check(guest::write_u64(addr, v));
    }
}

/// Writes an `i64` to `addr`. A null address is skipped.
pub(super) fn put_i64(addr: u64, v: i64) {
    if addr != 0 {
        check(guest::write_i64(addr, v));
    }
}

/// Writes an `f32` to `addr`. A null address is skipped.
pub(super) fn put_f32(addr: u64, v: f32) {
    if addr != 0 {
        check(guest::write_f32(addr, v));
    }
}

/// Writes bytes to `addr`. A null address is skipped.
pub(super) fn put_bytes(addr: u64, data: &[u8]) {
    if addr != 0 {
        check(guest::write(addr, data));
    }
}

/// Zeroes `len` bytes at `addr`. A null address is skipped.
pub(super) fn put_zero(addr: u64, len: u64) {
    if addr != 0 {
        check(guest::fill(addr, len, 0));
    }
}

/// Reads a `u8` at `addr`.
pub(super) fn get_u8(addr: u64) -> u8 {
    check(guest::read_u8(addr))
}

/// Reads a `u16` at `addr`.
pub(super) fn get_u16(addr: u64) -> u16 {
    check(guest::read_u16(addr))
}

/// Reads a `u32` at `addr`.
pub(super) fn get_u32(addr: u64) -> u32 {
    check(guest::read_u32(addr))
}

/// Reads an `i32` at `addr`.
pub(super) fn get_i32(addr: u64) -> i32 {
    check(guest::read_i32(addr))
}

/// Reads a `u64` at `addr`.
pub(super) fn get_u64(addr: u64) -> u64 {
    check(guest::read_u64(addr))
}

/// Reads an `i64` at `addr`.
pub(super) fn get_i64(addr: u64) -> i64 {
    check(guest::read_i64(addr))
}

/// Reads `len` bytes at `addr`.
pub(super) fn get_bytes(addr: u64, len: usize) -> Vec<u8> {
    let mut v = vec![0u8; len];
    check(guest::read(addr, &mut v));
    v
}

/// Reads a NUL-terminated string of at most `max` bytes at `addr`, lossily as UTF-8. A null address gives an empty
/// string.
pub(super) fn get_cstr(addr: u64, max: usize) -> String {
    if addr == 0 {
        return String::new();
    }
    String::from_utf8_lossy(&check(guest::read_cstr(addr, max))).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn the_names_are_unique() {
        let mut seen = BTreeSet::new();
        for e in all().into_iter().chain(big_stack()) {
            assert!(seen.insert(e.name), "{} is listed twice", e.name);
        }
    }

    #[test]
    fn every_export_has_an_address() {
        for e in all().into_iter().chain(big_stack()) {
            assert!(e.addr != 0, "{}", e.name);
        }
    }

    #[test]
    fn the_vsync_event_belongs_to_the_pacing_crate() {
        assert!(all().iter().all(|e| !e.name.contains("GetDisplayVsyncEvent")));
    }
}
