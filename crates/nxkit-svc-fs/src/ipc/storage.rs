//! `IStorage` over the romfs image and `ISaveDataInfoReader`.

use super::{SPACE, answer, arg_u64, out_buffer};
use crate::image::Image;
use crate::result::FAIL;
use nxkit_abi::guest;
use nxkit_abi::ipc::{Handled, Request, Response, Service};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The romfs as a storage. Reading the storage reads the image of the romfs folder (module `image`).
pub(super) struct IStorage {
    image: Arc<Image>,
}

impl IStorage {
    pub(super) fn new(image: Arc<Image>) -> IStorage {
        IStorage { image }
    }
}

impl Service for IStorage {
    fn name(&self) -> &'static str {
        "IStorage"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            // Read: offset s64 at 0, length s64 at 8, the bytes go to the receive buffer
            0 => {
                let (Ok(offset), Ok(length), Ok(buf)) = (arg_u64(req, 0), arg_u64(req, 8), out_buffer(req, 0)) else {
                    return answer(rsp, FAIL);
                };
                let n = length.min(buf.size);
                let read = guest::with_slice_mut(buf.addr, n, |dst| self.image.read(offset, dst));
                rsp.result = match read {
                    Ok(Ok(())) => 0,
                    _ => FAIL,
                };
                Handled::Done
            }
            4 => {
                rsp.push_u64(self.image.size());
                Handled::Done
            }
            _ => Handled::Unknown,
        }
    }
}

/// The size of one `SaveDataInfo` of `ReadSaveDataInfo`.
const INFO_SIZE: usize = 0x60;

/// The reader of the save data of one space. It lists the account save of the game once.
pub(super) struct ISaveDataInfoReader {
    entry: Option<[u8; INFO_SIZE]>,
    done: AtomicBool,
}

impl ISaveDataInfoReader {
    /// A reader that lists the account save of the program `title_id`, or nothing when `save` is `None`.
    pub(super) fn new(title_id: u64, save: bool) -> ISaveDataInfoReader {
        let entry = save.then(|| {
            let mut e = [0u8; INFO_SIZE];
            e[0x00..0x08].copy_from_slice(&1u64.to_le_bytes());
            e[0x08] = 1; // space id: user
            e[0x09] = 1; // type: account
            e[0x28..0x30].copy_from_slice(&title_id.to_le_bytes());
            e[0x30..0x38].copy_from_slice(&SPACE.to_le_bytes());
            e
        });
        ISaveDataInfoReader { entry, done: AtomicBool::new(false) }
    }
}

impl Service for ISaveDataInfoReader {
    fn name(&self) -> &'static str {
        "ISaveDataInfoReader"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        if req.command != 0 {
            return Handled::Unknown;
        }
        let Ok(buf) = out_buffer(req, 0) else { return answer(rsp, FAIL) };
        let listed = match &self.entry {
            Some(e) if buf.size >= INFO_SIZE as u64 && !self.done.swap(true, Ordering::Relaxed) => {
                if guest::write(buf.addr, e).is_err() {
                    return answer(rsp, FAIL);
                }
                1u64
            }
            _ => 0,
        };
        rsp.push_u64(listed);
        Handled::Done
    }
}
