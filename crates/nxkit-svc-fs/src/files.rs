//! Open files and open directories.
//!
//! A file of a read-only mount (the romfs, add-on content) is mapped, and a read copies from the mapping into the
//! guest buffer, so the bytes pass through the page cache only. A file of a writable mount is read and written with
//! `pread` and `pwrite` straight into and out of the guest buffer. Neither path allocates per call.

use crate::commit::Txn;
use crate::mount::Fs;
use crate::names::{self, MAX_NAME};
use crate::result::{FAIL, FsResult, OK, PATH_NOT_FOUND, from_io};
use crate::sys::Map;
use crate::table::lock;
use nxkit_abi::guest;
use std::fs::{OpenOptions, ReadDir};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// `nn::fs::OpenMode_Read`.
pub(crate) const MODE_READ: u32 = 1;
/// `nn::fs::OpenMode_Write`.
pub(crate) const MODE_WRITE: u32 = 2;
/// `nn::fs::OpenMode_AllowAppend`.
pub(crate) const MODE_APPEND: u32 = 4;

/// Directory filter bit: list directories.
pub(crate) const DIR_DIRECTORIES: u32 = 1;
/// Directory filter bit: list files.
pub(crate) const DIR_FILES: u32 = 2;

/// The size of one `nn::fs::DirectoryEntry`.
pub(crate) const ENTRY_SIZE: usize = 0x310;

enum Backing {
    Mapped(Map),
    Host { file: std::fs::File, txn: Option<Arc<Txn>> },
}

/// An open file.
pub(crate) struct File {
    mode: u32,
    backing: Backing,
}

fn guest_fault(_: guest::Fault) -> u32 {
    FAIL
}

impl File {
    /// The mode the game opened the file with.
    pub(crate) fn mode(&self) -> u32 {
        self.mode
    }

    /// The size of the file.
    pub(crate) fn size(&self) -> FsResult<u64> {
        match &self.backing {
            Backing::Mapped(m) => Ok(m.as_slice().len() as u64),
            Backing::Host { file, .. } => file.metadata().map(|m| m.len()).map_err(|e| from_io(&e)),
        }
    }

    /// Reads up to `size` bytes at `offset` into the guest buffer at `addr` and returns the count. A read past the end
    /// gives fewer bytes or none.
    pub(crate) fn read(&self, offset: i64, addr: u64, size: u64) -> FsResult<u64> {
        let offset = u64::try_from(offset).map_err(|_| FAIL)?;
        let len = self.size()?;
        if offset >= len || size == 0 {
            return Ok(0);
        }
        let n = size.min(len - offset);
        match &self.backing {
            Backing::Mapped(m) => {
                let start = usize::try_from(offset).map_err(|_| FAIL)?;
                let src = m.as_slice().get(start..start + n as usize).ok_or(FAIL)?;
                guest::with_slice_mut(addr, n, |dst| dst.copy_from_slice(src)).map_err(guest_fault)?;
            }
            Backing::Host { file, .. } => {
                guest::with_slice_mut(addr, n, |dst| read_full(file, dst, offset))
                    .map_err(guest_fault)?
                    .map_err(|e| from_io(&e))?;
            }
        }
        Ok(n)
    }

    /// Writes `size` bytes from the guest buffer at `addr` to `offset`. A write that fails poisons the save, so the
    /// half-written state is never committed.
    pub(crate) fn write(&self, offset: i64, addr: u64, size: u64) -> u32 {
        let Backing::Host { file, txn } = &self.backing else { return FAIL };
        if self.mode & (MODE_WRITE | MODE_APPEND) == 0 {
            return FAIL;
        }
        let _guard = txn.as_ref().map(|t| t.writing());
        let at = if self.mode & MODE_APPEND != 0 {
            match file.metadata() {
                Ok(m) => m.len(),
                Err(_) => return self.failed_write(),
            }
        } else {
            match u64::try_from(offset) {
                Ok(o) => o,
                Err(_) => return FAIL,
            }
        };
        if size == 0 {
            return OK;
        }
        match guest::with_slice(addr, size, |src| file.write_all_at(src, at)) {
            Ok(Ok(())) => OK,
            Ok(Err(_)) => self.failed_write(),
            Err(_) => FAIL,
        }
    }

    /// Sets the size of the file.
    pub(crate) fn set_size(&self, size: i64) -> u32 {
        let Backing::Host { file, txn } = &self.backing else { return FAIL };
        let Ok(size) = u64::try_from(size) else { return FAIL };
        if self.mode & (MODE_WRITE | MODE_APPEND) == 0 {
            return FAIL;
        }
        let _guard = txn.as_ref().map(|t| t.writing());
        match file.set_len(size) {
            Ok(()) => OK,
            Err(_) => self.failed_write(),
        }
    }

    fn failed_write(&self) -> u32 {
        if let Backing::Host { txn: Some(t), .. } = &self.backing {
            t.poison();
        }
        FAIL
    }
}

/// Fills `dst` from `offset`, as many bytes as the file has. The caller has clamped `dst` to the size of the file.
fn read_full(file: &std::fs::File, dst: &mut [u8], offset: u64) -> io::Result<()> {
    let mut done = 0usize;
    while done < dst.len() {
        match file.read_at(&mut dst[done..], offset + done as u64) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
            Ok(n) => done += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// An open directory.
pub(crate) struct Dir {
    path: PathBuf,
    mode: u32,
    iter: Mutex<ReadDir>,
}

/// One entry for the guest: the `nn::fs::DirectoryEntry` with the name, the type (0 directory, 1 file) and the size.
pub(crate) fn entry_bytes(name: &[u8], is_dir: bool, size: u64) -> FsResult<[u8; ENTRY_SIZE]> {
    if name.len() > MAX_NAME {
        return Err(FAIL);
    }
    let mut e = [0u8; ENTRY_SIZE];
    e[..name.len()].copy_from_slice(name);
    e[0x304] = u8::from(!is_dir);
    e[0x308..0x310].copy_from_slice(&size.to_le_bytes());
    Ok(e)
}

impl Dir {
    fn wanted(&self, is_dir: bool) -> bool {
        self.mode & if is_dir { DIR_DIRECTORIES } else { DIR_FILES } != 0
    }

    /// The entry after the previous call, or `None` at the end.
    fn next_entry(&self, it: &mut ReadDir) -> FsResult<Option<[u8; ENTRY_SIZE]>> {
        for e in it.by_ref() {
            let physical = e.map_err(|e| from_io(&e))?.file_name();
            let host = self.path.join(&physical);
            let meta = std::fs::metadata(&host).map_err(|e| from_io(&e))?;
            if !self.wanted(meta.is_dir()) {
                continue;
            }
            let name = names::guest_name(&self.path, &physical).map_err(|e| from_io(&e))?;
            return entry_bytes(&name, meta.is_dir(), if meta.is_dir() { 0 } else { meta.len() }).map(Some);
        }
        Ok(None)
    }

    /// Writes up to `max` entries to the guest buffer at `addr` and returns the count.
    pub(crate) fn read(&self, max: i64, addr: u64) -> FsResult<i64> {
        let mut it = lock(&self.iter);
        let mut count = 0i64;
        while count < max {
            let Some(entry) = self.next_entry(&mut it)? else { break };
            guest::write(addr + count as u64 * ENTRY_SIZE as u64, &entry).map_err(guest_fault)?;
            count += 1;
        }
        Ok(count)
    }

    /// The number of entries the filter lists.
    pub(crate) fn count(&self) -> FsResult<i64> {
        let mut it = std::fs::read_dir(&self.path).map_err(|e| from_io(&e))?;
        let mut n = 0i64;
        while self.next_entry(&mut it)?.is_some() {
            n += 1;
        }
        Ok(n)
    }
}

impl Fs {
    /// Opens the file `path` with `mode` and returns its handle.
    pub(crate) fn open_file(&self, path: &[u8], mode: u32) -> FsResult<u64> {
        if mode & (MODE_READ | MODE_WRITE | MODE_APPEND) == 0 {
            return Err(FAIL);
        }
        let write = mode & (MODE_WRITE | MODE_APPEND) != 0;
        let (mount, host) = self.existing(path)?;
        if write && mount.ro {
            return Err(FAIL);
        }
        let meta = std::fs::metadata(&host).map_err(|e| from_io(&e))?;
        if meta.is_dir() {
            return Err(PATH_NOT_FOUND);
        }
        let backing = if mount.ro {
            let f = std::fs::File::open(&host).map_err(|e| from_io(&e))?;
            let len = usize::try_from(meta.len()).map_err(|_| FAIL)?;
            Backing::Mapped(Map::new(&f, len).map_err(|e| from_io(&e))?)
        } else {
            let f = OpenOptions::new()
                .read(mode & MODE_READ != 0)
                .write(write)
                .open(&host)
                .map_err(|e| from_io(&e))?;
            Backing::Host { file: f, txn: mount.txn.clone() }
        };
        Ok(self.files.insert(Arc::new(File { mode, backing })))
    }

    /// The open file of `handle`.
    pub(crate) fn file(&self, handle: u64) -> FsResult<Arc<File>> {
        self.files.get(handle).ok_or(FAIL)
    }

    /// Closes the file of `handle`.
    pub(crate) fn close_file(&self, handle: u64) -> u32 {
        if self.files.remove(handle).is_some() { OK } else { FAIL }
    }

    /// Opens the directory `path`. `mode` has the bits `DIR_DIRECTORIES` and `DIR_FILES`.
    pub(crate) fn open_dir(&self, path: &[u8], mode: u32) -> FsResult<u64> {
        if mode & (DIR_DIRECTORIES | DIR_FILES) == 0 {
            return Err(FAIL);
        }
        let (_, host) = self.existing(path)?;
        if !host.is_dir() {
            return Err(PATH_NOT_FOUND);
        }
        let iter = std::fs::read_dir(&host).map_err(|e| from_io(&e))?;
        Ok(self.dirs.insert(Arc::new(Dir { path: host, mode, iter: Mutex::new(iter) })))
    }

    /// The open directory of `handle`.
    pub(crate) fn dir(&self, handle: u64) -> FsResult<Arc<Dir>> {
        self.dirs.get(handle).ok_or(FAIL)
    }

    /// Closes the directory of `handle`.
    pub(crate) fn close_dir(&self, handle: u64) -> u32 {
        if self.dirs.remove(handle).is_some() { OK } else { FAIL }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Guest, config};

    #[test]
    fn an_entry_has_the_layout_of_the_sdk() {
        let e = entry_bytes(b"game_data.sav", false, 0x1234).unwrap();
        assert_eq!(&e[..13], b"game_data.sav");
        assert_eq!(e[13], 0);
        assert_eq!(e[0x304], 1);
        assert_eq!(&e[0x308..0x310], &0x1234u64.to_le_bytes());
        assert_eq!(entry_bytes(b"d", true, 0).unwrap()[0x304], 0);
        assert!(entry_bytes(&vec![b'a'; MAX_NAME + 1], false, 0).is_err());
    }

    #[test]
    fn a_closed_handle_is_refused() {
        let cfg = config("files-closed");
        std::fs::write(cfg.romfs.join("a.bin"), b"abc").unwrap();
        let fs = Fs::new(cfg);
        assert_eq!(fs.mount_rom(b"rom"), OK);
        let h = fs.open_file(b"rom:/a.bin", MODE_READ).unwrap();
        assert_eq!(fs.file(h).unwrap().size(), Ok(3));
        assert_eq!(fs.close_file(h), OK);
        assert_eq!(fs.close_file(h), FAIL);
        assert!(fs.file(h).is_err());
        assert!(fs.file(0).is_err());
    }

    #[test]
    fn a_write_open_of_a_read_only_mount_fails() {
        let cfg = config("files-ro");
        std::fs::write(cfg.romfs.join("a.bin"), b"abc").unwrap();
        let fs = Fs::new(cfg);
        assert_eq!(fs.mount_rom(b"rom"), OK);
        assert_eq!(fs.open_file(b"rom:/a.bin", MODE_WRITE).err(), Some(FAIL));
        assert_eq!(fs.open_file(b"rom:/missing.bin", MODE_READ).err(), Some(PATH_NOT_FOUND));
        assert_eq!(fs.open_file(b"rom:/a.bin", 0).err(), Some(FAIL));
    }

    #[test]
    fn a_directory_is_not_a_file() {
        let cfg = config("files-dir");
        std::fs::create_dir(cfg.romfs.join("d")).unwrap();
        let fs = Fs::new(cfg);
        assert_eq!(fs.mount_rom(b"rom"), OK);
        assert_eq!(fs.open_file(b"rom:/d", MODE_READ).err(), Some(PATH_NOT_FOUND));
        assert!(fs.open_dir(b"rom:/d", DIR_FILES).is_ok());
        assert_eq!(fs.open_dir(b"rom:/missing", DIR_FILES).err(), Some(PATH_NOT_FOUND));
    }

    #[test]
    fn the_listing_follows_the_filter() {
        let cfg = config("files-list");
        std::fs::create_dir(cfg.romfs.join("d")).unwrap();
        std::fs::write(cfg.romfs.join("f1"), b"1").unwrap();
        std::fs::write(cfg.romfs.join("f2"), b"22").unwrap();
        let fs = Fs::new(cfg);
        assert_eq!(fs.mount_rom(b"rom"), OK);
        let count = |mode| {
            let h = fs.open_dir(b"rom:/", mode).unwrap();
            let n = fs.dir(h).unwrap().count().unwrap();
            assert_eq!(fs.close_dir(h), OK);
            n
        };
        assert_eq!(count(DIR_DIRECTORIES), 1);
        assert_eq!(count(DIR_FILES), 2);
        assert_eq!(count(DIR_DIRECTORIES | DIR_FILES), 3);
    }

    #[test]
    fn a_mapped_file_is_read_into_the_guest_buffer() {
        let cfg = config("files-read");
        std::fs::write(cfg.romfs.join("a.bin"), b"0123456789").unwrap();
        let fs = Fs::new(cfg);
        assert_eq!(fs.mount_rom(b"rom"), OK);
        let g = Guest::new(64);
        let f = fs.file(fs.open_file(b"rom:/a.bin", MODE_READ).unwrap()).unwrap();
        assert_eq!(f.read(2, g.addr(), 4), Ok(4));
        assert_eq!(g.bytes(0, 4), b"2345");
        assert_eq!(f.read(8, g.addr(), 16), Ok(2));
        assert_eq!(g.bytes(0, 2), b"89");
        assert_eq!(f.read(10, g.addr(), 4), Ok(0));
        assert_eq!(f.read(-1, g.addr(), 4), Err(FAIL));
        assert_eq!(f.read(0, g.addr() + 1000, 4), Err(FAIL));
    }

    #[test]
    fn a_host_file_is_written_read_and_resized() {
        let fs = Fs::new(config("files-write"));
        assert_eq!(fs.mount_temp(b"tmp"), OK);
        std::fs::write(fs.cfg.temp.join("w.bin"), b"").unwrap();
        let g = Guest::new(64);
        g.put(0, b"hello");
        let h = fs.open_file(b"tmp:/w.bin", MODE_READ | MODE_WRITE).unwrap();
        let f = fs.file(h).unwrap();
        assert_eq!(f.write(3, g.addr(), 5), OK);
        assert_eq!(f.size(), Ok(8));
        assert_eq!(f.read(0, g.addr() + 16, 8), Ok(8));
        assert_eq!(g.bytes(16, 8), b"\0\0\0hello");
        assert_eq!(f.set_size(4), OK);
        assert_eq!(f.size(), Ok(4));
        assert_eq!(f.write(-1, g.addr(), 1), FAIL);
        assert_eq!(f.mode(), MODE_READ | MODE_WRITE);
        let read_only = fs.file(fs.open_file(b"tmp:/w.bin", MODE_READ).unwrap()).unwrap();
        assert_eq!(read_only.write(0, g.addr(), 1), FAIL);
    }

    #[test]
    fn an_append_writes_at_the_end() {
        let fs = Fs::new(config("files-append"));
        assert_eq!(fs.mount_temp(b"tmp"), OK);
        std::fs::write(fs.cfg.temp.join("w.bin"), b"ab").unwrap();
        let g = Guest::new(16);
        g.put(0, b"cd");
        let f = fs.file(fs.open_file(b"tmp:/w.bin", MODE_WRITE | MODE_APPEND).unwrap()).unwrap();
        assert_eq!(f.write(0, g.addr(), 2), OK);
        assert_eq!(std::fs::read(fs.cfg.temp.join("w.bin")).unwrap(), b"abcd");
    }
}
