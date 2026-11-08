//! The root interface `fsp-srv`.

use super::filesystem::IFileSystem;
use super::storage::{ISaveDataInfoReader, IStorage};
use super::{SPACE, answer, service};
use crate::image::Image;
use crate::mount::Fs;
use crate::result::{FAIL, OK, TARGET_NOT_FOUND};
use nxkit_abi::ipc::{Handled, Request, Response, Service};
use std::sync::{Arc, OnceLock};

/// The `SaveDataSpaceId` of the user save data.
const SPACE_USER: u8 = 1;

/// `SaveDataType` values of the attribute.
const TYPE_ACCOUNT: u8 = 1;
const TYPE_DEVICE: u8 = 3;
const TYPE_CACHE: u8 = 5;

/// The `fsp-srv` service.
pub(crate) struct FspSrv {
    image: OnceLock<Option<Arc<Image>>>,
}

impl FspSrv {
    pub(crate) fn new() -> FspSrv {
        FspSrv {
            image: OnceLock::new(),
        }
    }

    /// The image of the romfs, built at the first use. A failure is logged and stays a failure.
    fn image(&self, fs: &Fs) -> Option<Arc<Image>> {
        self.image
            .get_or_init(|| match Image::build(&fs.cfg.romfs) {
                Ok(i) => Some(Arc::new(i)),
                Err(e) => {
                    eprintln!(
                        "[RED] nxkit-svc-fs: the romfs image of {} cannot be built: {e}",
                        fs.cfg.romfs.display()
                    );
                    None
                }
            })
            .clone()
    }

    fn data_storage(&self, rsp: &mut Response) -> Handled {
        let Some(fs) = service(rsp) else {
            return Handled::Done;
        };
        match self.image(fs) {
            Some(img) => {
                rsp.objects.push(Arc::new(IStorage::new(img)));
                Handled::Done
            }
            None => answer(rsp, FAIL),
        }
    }

    /// `OpenSaveDataFileSystem`: space id u8 at 0, then the attribute at 8 (program id, user id, system save data id,
    /// type u8 at 0x20, rank u8, index u16 at 0x22).
    fn open_save(req: &Request<'_>, rsp: &mut Response) -> Handled {
        let Some(fs) = service(rsp) else {
            return Handled::Done;
        };
        let Some(attr) = req.data.get(8..0x48) else {
            return answer(rsp, FAIL);
        };
        let (kind, index) = (attr[0x20], u16::from_le_bytes([attr[0x22], attr[0x23]]));
        let (name, r) = match kind {
            TYPE_ACCOUNT => (b"@save".to_vec(), fs.mount_save(b"@save", &fs.save_dir())),
            TYPE_DEVICE if fs.cfg.separate_device_save => (
                b"@device".to_vec(),
                fs.mount_save(b"@device", &fs.device_save_dir()),
            ),
            TYPE_DEVICE => (b"@save".to_vec(), fs.mount_save(b"@save", &fs.save_dir())),
            TYPE_CACHE => {
                let name = format!("@cache{index}").into_bytes();
                let r = fs.mount_cache(&name, i32::from(index));
                (name, r)
            }
            other => {
                eprintln!(
                    "[RED] nxkit-svc-fs: OpenSaveDataFileSystem of save type {other}: the host serves account, device and cache saves only"
                );
                return answer(rsp, TARGET_NOT_FOUND);
            }
        };
        if r != OK {
            return answer(rsp, r);
        }
        rsp.objects.push(Arc::new(IFileSystem::new(name)));
        Handled::Done
    }
}

impl Service for FspSrv {
    fn name(&self) -> &'static str {
        "fsp-srv"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            // SetCurrentProcess, CreateSaveDataFileSystem and ExtendSaveDataFileSystem: the save folder is made when it
            // is mounted and has no size limit.
            1 | 22 | 32 => answer(rsp, OK),
            33 => {
                let Some(fs) = service(rsp) else {
                    return Handled::Done;
                };
                match req.u32_at(0) {
                    Some(index) => answer(rsp, fs.delete_cache(index as i32)),
                    None => answer(rsp, FAIL),
                }
            }
            34 => {
                rsp.push_u64(SPACE);
                rsp.push_u64(SPACE);
                Handled::Done
            }
            51 => Self::open_save(req, rsp),
            53 => {
                eprintln!(
                    "[RED] nxkit-svc-fs: read-only open of save data: the host has none for another program"
                );
                answer(rsp, TARGET_NOT_FOUND)
            }
            61 => {
                let Some(fs) = service(rsp) else {
                    return Handled::Done;
                };
                let user = req.data.first() == Some(&SPACE_USER);
                let reader =
                    ISaveDataInfoReader::new(fs.cfg.title_id, user && fs.save_dir().is_dir());
                rsp.objects.push(Arc::new(reader));
                Handled::Done
            }
            // The merged romfs serves as the data storage and as the patch data storage.
            200 | 203 => self.data_storage(rsp),
            // SetSaveDataSize and DisableAutoSaveDataCreation
            1001 | 1003 => answer(rsp, OK),
            // GetGlobalAccessLogMode: access logging is off
            1005 => {
                rsp.push_u32(0);
                Handled::Done
            }
            // OutputAccessLogToSdCard
            1006 => answer(rsp, OK),
            _ => Handled::Unknown,
        }
    }
}
