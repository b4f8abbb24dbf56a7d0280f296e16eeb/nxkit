//! The `nn::fs` functions at SDK level (R14).
//!
//! The game calls these functions of the SDK, and the host answers them in place of the SDK. The set is the one
//! `dm/host/hle_fs.c` hooks, under the same mangled names. The IPC interfaces of `fsp-srv` (module `ipc`) serve a game
//! that reaches the service in another way.
//!
//! The functions are plain `extern "C"` functions. A pointer the game passes is a guest address that is read and
//! written through `nxkit_abi::guest`, so a bad address gives a failure result and never a fault of the host.

mod file;
mod mount;
mod table;

use crate::mount::Fs;
use crate::names::MAX_NAME;
use crate::result::{FAIL, OK};
use nxkit_abi::export::Export;
use nxkit_abi::guest;

/// The exports that run on the stack of the calling guest thread.
pub(crate) fn exports() -> Vec<Export> {
    table::entries()
}

/// The exports that run on the big host stack. Every take-over of `hle_fs.c` is one of these.
pub(crate) fn big_exports() -> Vec<Export> {
    table::big_entries()
}

/// The path or name at the guest address `addr`. A string longer than Horizon's limit is refused.
fn text(addr: u64) -> Result<Vec<u8>, u32> {
    match guest::read_cstr(addr, MAX_NAME + 1) {
        Ok(v) if v.len() <= MAX_NAME => Ok(v),
        _ => Err(FAIL),
    }
}

fn done(r: Result<(), guest::Fault>) -> u32 {
    if r.is_ok() { OK } else { FAIL }
}

/// Stores `v` at `addr`.
fn put_u64(addr: u64, v: u64) -> u32 {
    done(guest::write_u64(addr, v))
}

fn put_i64(addr: u64, v: i64) -> u32 {
    done(guest::write_i64(addr, v))
}

fn put_i32(addr: u64, v: i32) -> u32 {
    done(guest::write_i32(addr, v))
}

fn put_u32(addr: u64, v: u32) -> u32 {
    done(guest::write_u32(addr, v))
}

fn put_u8(addr: u64, v: u8) -> u32 {
    done(guest::write_u8(addr, v))
}

/// Stores `v` at `addr` unless the game passed a null pointer for an output it does not want.
fn put_i64_opt(addr: u64, v: i64) -> u32 {
    if addr == 0 { OK } else { put_i64(addr, v) }
}

/// Runs `f` with the service, or fails and says so when the host did not start it.
fn run(f: impl FnOnce(&Fs) -> u32) -> u32 {
    match crate::fs() {
        Some(fs) => f(fs),
        None => FAIL,
    }
}

/// Runs `f` with the text at `addr`.
fn run_text(addr: u64, f: impl FnOnce(&Fs, &[u8]) -> u32) -> u32 {
    match text(addr) {
        Ok(t) => run(|fs| f(fs, &t)),
        Err(r) => r,
    }
}

#[cfg(test)]
mod tests;
