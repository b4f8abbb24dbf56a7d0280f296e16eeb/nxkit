//! `IFile` and `IDirectory`, each over a handle of the service. Dropping the interface closes the handle.

use super::{answer, arg_u64, in_buffer, out_buffer};
use crate::files::ENTRY_SIZE;
use crate::mount::Fs;
use crate::result::FAIL;
use nxkit_abi::ipc::{Handled, Request, Response, Service};

/// An open file.
pub(super) struct IFile {
    fs: &'static Fs,
    handle: u64,
}

impl IFile {
    pub(super) fn new(fs: &'static Fs, handle: u64) -> IFile {
        IFile { fs, handle }
    }
}

impl Drop for IFile {
    fn drop(&mut self) {
        self.fs.close_file(self.handle);
    }
}

impl Service for IFile {
    fn name(&self) -> &'static str {
        "IFile"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        let file = match self.fs.file(self.handle) {
            Ok(f) => f,
            Err(r) => return answer(rsp, r),
        };
        match req.command {
            // Read and Write: option u32 at 0, offset s64 at 8, size s64 at 16
            0 => {
                let (Ok(offset), Ok(size), Ok(buf)) = (arg_u64(req, 8), arg_u64(req, 16), out_buffer(req, 0)) else {
                    return answer(rsp, FAIL);
                };
                match file.read(offset as i64, buf.addr, size.min(buf.size)) {
                    Ok(n) => rsp.push_u64(n),
                    Err(r) => rsp.result = r,
                }
                Handled::Done
            }
            1 => {
                let (Ok(offset), Ok(size), Ok(buf)) = (arg_u64(req, 8), arg_u64(req, 16), in_buffer(req, 0)) else {
                    return answer(rsp, FAIL);
                };
                answer(rsp, file.write(offset as i64, buf.addr, size.min(buf.size)))
            }
            2 => answer(rsp, 0),
            3 => match arg_u64(req, 0) {
                Ok(size) => answer(rsp, file.set_size(size as i64)),
                Err(r) => answer(rsp, r),
            },
            4 => {
                match file.size() {
                    Ok(n) => rsp.push_u64(n),
                    Err(r) => rsp.result = r,
                }
                Handled::Done
            }
            _ => Handled::Unknown,
        }
    }
}

/// An open directory.
pub(super) struct IDirectory {
    fs: &'static Fs,
    handle: u64,
}

impl IDirectory {
    pub(super) fn new(fs: &'static Fs, handle: u64) -> IDirectory {
        IDirectory { fs, handle }
    }
}

impl Drop for IDirectory {
    fn drop(&mut self) {
        self.fs.close_dir(self.handle);
    }
}

impl Service for IDirectory {
    fn name(&self) -> &'static str {
        "IDirectory"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        let dir = match self.fs.dir(self.handle) {
            Ok(d) => d,
            Err(r) => return answer(rsp, r),
        };
        match req.command {
            // Read: the entries go to the receive buffer, the count is the answer
            0 => {
                let Ok(buf) = out_buffer(req, 0) else { return answer(rsp, FAIL) };
                match dir.read((buf.size / ENTRY_SIZE as u64) as i64, buf.addr) {
                    Ok(n) => rsp.push_u64(n as u64),
                    Err(r) => rsp.result = r,
                }
                Handled::Done
            }
            1 => {
                match dir.count() {
                    Ok(n) => rsp.push_u64(n as u64),
                    Err(r) => rsp.result = r,
                }
                Handled::Done
            }
            _ => Handled::Unknown,
        }
    }
}
