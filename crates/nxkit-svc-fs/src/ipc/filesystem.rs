//! `IFileSystem`, backed by a mount of the service.

use super::{SPACE, answer, arg_u64, path_arg, service};
use crate::files::{DIR_DIRECTORIES, DIR_FILES};
use crate::ipc::objects::{IDirectory, IFile};
use crate::mount::Fs;
use crate::result::FAIL;
use nxkit_abi::ipc::{Handled, Request, Response, Service};
use std::sync::Arc;

/// An open file system: the mount `mount` of the service, which holds the paths of this interface.
pub(super) struct IFileSystem {
    mount: Vec<u8>,
}

impl IFileSystem {
    pub(super) fn new(mount: Vec<u8>) -> IFileSystem {
        IFileSystem { mount }
    }

    /// The path of the service for a path of this interface.
    fn full(&self, path: &[u8]) -> Vec<u8> {
        let mut v = Vec::with_capacity(self.mount.len() + 1 + path.len());
        v.extend_from_slice(&self.mount);
        v.push(b':');
        v.extend_from_slice(path);
        v
    }

    fn path(&self, req: &Request<'_>, n: u32) -> Result<Vec<u8>, u32> {
        path_arg(req, n).map(|p| self.full(&p))
    }

    fn with_path(
        &self,
        req: &Request<'_>,
        rsp: &mut Response,
        f: impl FnOnce(&Fs, &[u8]) -> u32,
    ) -> Handled {
        let Some(fs) = service(rsp) else {
            return Handled::Done;
        };
        rsp.result = match self.path(req, 0) {
            Ok(p) => f(fs, &p),
            Err(r) => r,
        };
        Handled::Done
    }

    fn open_file(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        let Some(fs) = service(rsp) else {
            return Handled::Done;
        };
        let (Some(mode), Ok(path)) = (req.u32_at(0), self.path(req, 0)) else {
            return answer(rsp, FAIL);
        };
        match fs.open_file(&path, mode) {
            Ok(handle) => {
                rsp.objects.push(Arc::new(IFile::new(fs, handle)));
                Handled::Done
            }
            Err(r) => answer(rsp, r),
        }
    }

    fn open_directory(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        let Some(fs) = service(rsp) else {
            return Handled::Done;
        };
        let (Some(filter), Ok(path)) = (req.u32_at(0), self.path(req, 0)) else {
            return answer(rsp, FAIL);
        };
        match fs.open_dir(&path, filter & (DIR_DIRECTORIES | DIR_FILES)) {
            Ok(handle) => {
                rsp.objects.push(Arc::new(IDirectory::new(fs, handle)));
                Handled::Done
            }
            Err(r) => answer(rsp, r),
        }
    }
}

impl Service for IFileSystem {
    fn name(&self) -> &'static str {
        "IFileSystem"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            0 => {
                // option u32 at 0, size s64 at 8
                let Ok(size) = arg_u64(req, 8) else {
                    return answer(rsp, FAIL);
                };
                self.with_path(req, rsp, |fs, p| fs.create_file(p, size as i64))
            }
            1 => self.with_path(req, rsp, |fs, p| fs.delete_file(p)),
            2 => self.with_path(req, rsp, |fs, p| fs.create_dir(p)),
            3 => self.with_path(req, rsp, |fs, p| fs.delete_dir(p)),
            4 => self.with_path(req, rsp, |fs, p| fs.delete_dir_recursively(p)),
            5 | 6 => {
                let Some(fs) = service(rsp) else {
                    return Handled::Done;
                };
                rsp.result = match (self.path(req, 0), self.path(req, 1)) {
                    (Ok(from), Ok(to)) => fs.rename(&from, &to, req.command == 6),
                    _ => FAIL,
                };
                Handled::Done
            }
            7 => {
                let Some(fs) = service(rsp) else {
                    return Handled::Done;
                };
                match self.path(req, 0).and_then(|p| fs.entry_type(&p)) {
                    Ok(t) => rsp.push_u32(t as u32),
                    Err(r) => rsp.result = r,
                }
                Handled::Done
            }
            8 => self.open_file(req, rsp),
            9 => self.open_directory(req, rsp),
            10 => {
                let Some(fs) = service(rsp) else {
                    return Handled::Done;
                };
                rsp.result = fs.commit(&self.mount);
                Handled::Done
            }
            11 | 12 => {
                rsp.push_u64(SPACE);
                Handled::Done
            }
            13 => self.with_path(req, rsp, |fs, p| fs.clean_dir_recursively(p)),
            _ => Handled::Unknown,
        }
    }
}
