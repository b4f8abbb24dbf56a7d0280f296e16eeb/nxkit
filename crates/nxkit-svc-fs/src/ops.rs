//! Operations on paths: create, delete, rename and the type of an entry.
//!
//! The result codes are the ones of `dm/host/hle_fs.c`. A write operation on a read-only mount fails, and a create on a
//! read-only mount says "exists" when the entry is there. An operation that changes the save takes the read side of the
//! gate of its transaction, so a commit never sees it half done. An operation that can stop in the middle poisons the
//! transaction when it fails, so the half state is not committed.

use crate::mount::{Fs, Mount};
use crate::names;
use crate::result::{FAIL, FsResult, OK, PATH_EXISTS, PATH_NOT_FOUND, from_io};
use std::fs::{DirBuilder, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;
use std::sync::Arc;

fn status(r: FsResult<()>) -> u32 {
    match r {
        Ok(()) => OK,
        Err(c) => c,
    }
}

fn poison(m: &Mount) {
    if let Some(t) = &m.txn {
        t.poison();
    }
}

impl Fs {
    /// A mount that can change, and the components of `path` in it. A read-only mount refuses.
    fn writable(&self, path: &[u8], creating: bool) -> FsResult<(Arc<Mount>, Vec<Vec<u8>>)> {
        let (mount, comps) = self.locate(path)?;
        if mount.ro {
            let there = Self::host_path(&mount, &comps, false).is_ok();
            return Err(if creating && there { PATH_EXISTS } else { FAIL });
        }
        Ok((mount, comps))
    }

    /// 0 for a directory, 1 for a file.
    pub(crate) fn entry_type(&self, path: &[u8]) -> FsResult<i32> {
        let (_, host) = self.existing(path)?;
        let meta = std::fs::metadata(host).map_err(|e| from_io(&e))?;
        Ok(i32::from(!meta.is_dir()))
    }

    /// Creates a file of `size` bytes. The content is zero.
    pub(crate) fn create_file(&self, path: &[u8], size: i64) -> u32 {
        status(self.try_create_file(path, size))
    }

    fn try_create_file(&self, path: &[u8], size: i64) -> FsResult<()> {
        let size = u64::try_from(size).map_err(|_| FAIL)?;
        let (m, comps) = self.writable(path, true)?;
        let last = comps.last().ok_or(PATH_EXISTS)?;
        let _guard = m.txn.as_ref().map(|t| t.writing());
        let host = Self::host_path(&m, &comps, true)?;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&host)
            .map_err(|e| from_io(&e))?;
        let done = file
            .set_len(size)
            .and_then(|()| names::set_guest_name(&host, last));
        if done.is_err() {
            drop(file);
            if std::fs::remove_file(&host).is_err() {
                poison(&m);
            }
            return Err(FAIL);
        }
        Ok(())
    }

    /// Creates a directory. The parent must exist.
    pub(crate) fn create_dir(&self, path: &[u8]) -> u32 {
        status(self.try_create_dir(path))
    }

    fn try_create_dir(&self, path: &[u8]) -> FsResult<()> {
        let (m, comps) = self.writable(path, true)?;
        let last = comps.last().ok_or(PATH_EXISTS)?;
        let _guard = m.txn.as_ref().map(|t| t.writing());
        let host = Self::host_path(&m, &comps, true)?;
        DirBuilder::new()
            .mode(0o755)
            .create(&host)
            .map_err(|e| from_io(&e))?;
        if names::set_guest_name(&host, last).is_err() {
            if std::fs::remove_dir(&host).is_err() {
                poison(&m);
            }
            return Err(FAIL);
        }
        Ok(())
    }

    /// Deletes a file.
    pub(crate) fn delete_file(&self, path: &[u8]) -> u32 {
        status(self.try_delete(path, false, false))
    }

    /// Deletes an empty directory.
    pub(crate) fn delete_dir(&self, path: &[u8]) -> u32 {
        status(self.try_delete(path, true, false))
    }

    /// Deletes a directory and all it holds.
    pub(crate) fn delete_dir_recursively(&self, path: &[u8]) -> u32 {
        status(self.try_delete(path, true, true))
    }

    fn try_delete(&self, path: &[u8], is_dir: bool, recursive: bool) -> FsResult<()> {
        let (m, comps) = self.writable(path, false)?;
        if comps.is_empty() {
            return Err(FAIL);
        }
        let _guard = m.txn.as_ref().map(|t| t.writing());
        let host = Self::host_path(&m, &comps, false)?;
        let meta = std::fs::symlink_metadata(&host).map_err(|e| from_io(&e))?;
        if meta.is_dir() != is_dir {
            return Err(PATH_NOT_FOUND);
        }
        let r = match (is_dir, recursive) {
            (false, _) => std::fs::remove_file(&host),
            (true, false) => std::fs::remove_dir(&host),
            (true, true) => std::fs::remove_dir_all(&host),
        };
        r.map_err(|e| {
            if recursive {
                poison(&m);
            }
            from_io(&e)
        })
    }

    /// Deletes everything in a directory and keeps the directory.
    pub(crate) fn clean_dir_recursively(&self, path: &[u8]) -> u32 {
        status(self.try_clean(path))
    }

    fn try_clean(&self, path: &[u8]) -> FsResult<()> {
        let (m, comps) = self.writable(path, false)?;
        let _guard = m.txn.as_ref().map(|t| t.writing());
        let host = Self::host_path(&m, &comps, false)?;
        if !host.is_dir() {
            return Err(PATH_NOT_FOUND);
        }
        let result = clean(&host);
        if result.is_err() {
            poison(&m);
        }
        result.map_err(|e| from_io(&e))
    }

    /// Renames a file or, with `is_dir`, a directory. The target must not exist and must be in the same mount.
    pub(crate) fn rename(&self, from: &[u8], to: &[u8], is_dir: bool) -> u32 {
        status(self.try_rename(from, to, is_dir))
    }

    fn try_rename(&self, from: &[u8], to: &[u8], is_dir: bool) -> FsResult<()> {
        let (m, from_comps) = self.writable(from, false)?;
        let (m2, to_comps) = self.locate(to)?;
        if !Arc::ptr_eq(&m, &m2) || from_comps.is_empty() {
            return Err(FAIL);
        }
        let to_last = to_comps.last().ok_or(PATH_EXISTS)?;
        let _guard = m.txn.as_ref().map(|t| t.writing());
        let src = Self::host_path(&m, &from_comps, false)?;
        if std::fs::symlink_metadata(&src)
            .map_err(|e| from_io(&e))?
            .is_dir()
            != is_dir
        {
            return Err(PATH_NOT_FOUND);
        }
        if Self::host_path(&m, &to_comps, false).is_ok() {
            return Err(PATH_EXISTS);
        }
        let dst = Self::host_path(&m, &to_comps, true)?;
        std::fs::rename(&src, &dst).map_err(|e| from_io(&e))?;
        if names::set_guest_name(&dst, to_last).is_err() {
            if std::fs::rename(&dst, &src).is_err() {
                poison(&m);
            }
            return Err(FAIL);
        }
        Ok(())
    }
}

fn clean(dir: &Path) -> io::Result<()> {
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        if std::fs::symlink_metadata(&p)?.is_dir() {
            std::fs::remove_dir_all(&p)?;
        } else {
            std::fs::remove_file(&p)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::config;

    fn temp() -> Fs {
        let fs = Fs::new(config("ops"));
        assert_eq!(fs.mount_temp(b"tmp"), OK);
        fs
    }

    #[test]
    fn a_file_is_created_once() {
        let fs = temp();
        assert_eq!(fs.create_file(b"tmp:/a.sav", 16), OK);
        assert_eq!(
            std::fs::metadata(fs.cfg.temp.join("a.sav")).unwrap().len(),
            16
        );
        assert_eq!(fs.create_file(b"tmp:/a.sav", 16), PATH_EXISTS);
        assert_eq!(fs.create_file(b"tmp:/missing/a.sav", 0), PATH_NOT_FOUND);
        assert_eq!(fs.create_file(b"tmp:/b.sav", -1), FAIL);
        assert_eq!(fs.create_file(b"tmp:/", 0), PATH_EXISTS);
        assert_eq!(fs.entry_type(b"tmp:/a.sav"), Ok(1));
    }

    #[test]
    fn directories_are_created_listed_and_deleted() {
        let fs = temp();
        assert_eq!(fs.create_dir(b"tmp:/d"), OK);
        assert_eq!(fs.create_dir(b"tmp:/d"), PATH_EXISTS);
        assert_eq!(fs.create_file(b"tmp:/d/f", 1), OK);
        assert_eq!(fs.entry_type(b"tmp:/d"), Ok(0));
        assert_eq!(fs.delete_dir(b"tmp:/d"), FAIL);
        assert_eq!(fs.delete_file(b"tmp:/d"), PATH_NOT_FOUND);
        assert_eq!(fs.clean_dir_recursively(b"tmp:/d"), OK);
        assert!(fs.cfg.temp.join("d").is_dir());
        assert_eq!(fs.delete_dir(b"tmp:/d"), OK);
        assert_eq!(fs.entry_type(b"tmp:/d"), Err(PATH_NOT_FOUND));
        assert_eq!(fs.create_dir(b"tmp:/e"), OK);
        assert_eq!(fs.create_file(b"tmp:/e/f", 1), OK);
        assert_eq!(fs.delete_dir_recursively(b"tmp:/e"), OK);
        assert!(!fs.cfg.temp.join("e").exists());
        assert_eq!(fs.delete_dir(b"tmp:/"), FAIL);
    }

    #[test]
    fn a_file_is_deleted() {
        let fs = temp();
        assert_eq!(fs.create_file(b"tmp:/a", 0), OK);
        assert_eq!(fs.delete_file(b"tmp:/a"), OK);
        assert_eq!(fs.delete_file(b"tmp:/a"), PATH_NOT_FOUND);
    }

    #[test]
    fn a_rename_needs_a_free_target_in_the_same_mount() {
        let fs = temp();
        assert_eq!(fs.mount_cache(b"cache", 0), OK);
        assert_eq!(fs.create_file(b"tmp:/a", 0), OK);
        assert_eq!(fs.create_file(b"tmp:/b", 0), OK);
        assert_eq!(fs.rename(b"tmp:/a", b"tmp:/b", false), PATH_EXISTS);
        assert_eq!(fs.rename(b"tmp:/a", b"cache:/a", false), FAIL);
        assert_eq!(fs.rename(b"tmp:/a", b"tmp:/c", true), PATH_NOT_FOUND);
        assert_eq!(fs.rename(b"tmp:/a", b"tmp:/c", false), OK);
        assert!(fs.cfg.temp.join("c").is_file());
        assert!(!fs.cfg.temp.join("a").exists());
        assert_eq!(fs.rename(b"tmp:/missing", b"tmp:/d", false), PATH_NOT_FOUND);
    }

    #[test]
    fn a_read_only_mount_refuses_changes() {
        let cfg = config("ops-ro");
        std::fs::write(cfg.romfs.join("a.bin"), b"x").unwrap();
        let fs = Fs::new(cfg);
        assert_eq!(fs.mount_rom(b"rom"), OK);
        assert_eq!(fs.create_file(b"rom:/a.bin", 0), PATH_EXISTS);
        assert_eq!(fs.create_file(b"rom:/new.bin", 0), FAIL);
        assert_eq!(fs.create_dir(b"rom:/new"), FAIL);
        assert_eq!(fs.delete_file(b"rom:/a.bin"), FAIL);
        assert_eq!(fs.rename(b"rom:/a.bin", b"rom:/b.bin", false), FAIL);
        assert_eq!(fs.clean_dir_recursively(b"rom:/"), FAIL);
        assert!(fs.cfg.romfs.join("a.bin").is_file());
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn names_that_differ_in_case_are_kept_apart() {
        let fs = temp();
        assert_eq!(fs.create_file(b"tmp:/Data.bin", 0), OK);
        assert_eq!(fs.create_file(b"tmp:/data.bin", 0), OK);
        assert_eq!(fs.entry_type(b"tmp:/Data.bin"), Ok(1));
        assert_eq!(fs.entry_type(b"tmp:/data.bin"), Ok(1));
        assert_eq!(fs.entry_type(b"tmp:/DATA.bin"), Err(PATH_NOT_FOUND));
        assert_eq!(fs.rename(b"tmp:/data.bin", b"tmp:/Other.bin", false), OK);
        assert_eq!(fs.entry_type(b"tmp:/Other.bin"), Ok(1));
        assert_eq!(fs.delete_file(b"tmp:/Data.bin"), OK);
        assert_eq!(fs.entry_type(b"tmp:/Data.bin"), Err(PATH_NOT_FOUND));
    }
}
