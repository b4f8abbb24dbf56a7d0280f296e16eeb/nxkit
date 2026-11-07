//! The IPC interfaces of `fsp-srv` (R35).
//!
//! The command numbers, the layout of the raw data and the buffer kinds are those of the Horizon `fssrv` interfaces
//! as the homebrew libraries describe them. They are not verified against hardware or against nxnative, which has no
//! `fsp-srv` server because it takes the `nn::fs` functions over at SDK level. A command that is not listed here
//! gives `Handled::Unknown`, which stops the game with the interface and the command ID (R15).
//!
//! The paths of an `IFileSystem` have no mount name. Each `IFileSystem` is backed by a mount of the same service
//! that has a fixed name starting with `@`, so the SDK functions and the interfaces share one save transaction.

mod filesystem;
mod fsp;
mod objects;
mod storage;

pub(crate) use fsp::FspSrv;

use crate::mount::Fs;
use crate::names::MAX_NAME;
use crate::result::FAIL;
use nxkit_abi::guest;
use nxkit_abi::ipc::{Buffer, BufferKind, Handled, Request, Response};

/// The size reported for the free and total space of a save, as `GetFreeSpaceSize` does at SDK level.
const SPACE: u64 = 256 << 20;

/// Answers with the result `r` and no data.
fn answer(rsp: &mut Response, r: u32) -> Handled {
    rsp.result = r;
    Handled::Done
}

/// The service, or the failure answer when the host did not start it.
fn service(rsp: &mut Response) -> Option<&'static Fs> {
    let fs = crate::fs();
    if fs.is_none() {
        rsp.result = FAIL;
    }
    fs
}

/// The path in buffer `n`. A pointer buffer is the usual case, and a send buffer is accepted too.
fn path_arg(req: &Request<'_>, n: u32) -> Result<Vec<u8>, u32> {
    let b = req.buffer(BufferKind::Pointer, n).or_else(|| req.buffer(BufferKind::Send, n)).ok_or(FAIL)?;
    let max = usize::try_from(b.size).unwrap_or(usize::MAX).min(MAX_NAME + 1);
    match guest::read_cstr(b.addr, max) {
        Ok(v) if v.len() <= MAX_NAME => Ok(v),
        _ => Err(FAIL),
    }
}

/// The buffer the game receives data in.
fn out_buffer(req: &Request<'_>, n: u32) -> Result<Buffer, u32> {
    req.buffer(BufferKind::Receive, n).or_else(|| req.buffer(BufferKind::ReceiveList, n)).ok_or(FAIL)
}

/// The buffer the game sends data in.
fn in_buffer(req: &Request<'_>, n: u32) -> Result<Buffer, u32> {
    req.buffer(BufferKind::Send, n).ok_or(FAIL)
}

/// A 64-bit value of the raw data, or the failure result when the data is too short.
fn arg_u64(req: &Request<'_>, offset: usize) -> Result<u64, u32> {
    req.u64_at(offset).ok_or(FAIL)
}

#[cfg(test)]
mod tests;
