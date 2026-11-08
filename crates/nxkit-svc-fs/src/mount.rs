//! The mounts of the host: the romfs, add-on content, temporary and cache storage, and the saves.
//!
//! The table of mounts is permanent, as in `dm/host/hle_fs.c`: a name that is mounted stays mounted, and a second
//! mount of the same name returns success. The table is copy-on-write, so a lookup takes a read lock for the time of
//! one `Arc` clone and mounting, which is rare, takes a lock of its own.

use crate::Config;
use crate::commit::Txn;
use crate::files::{Dir, File};
use crate::names::{self, IndexCache};
use crate::result::{FAIL, FsResult, OK, PATH_NOT_FOUND, TARGET_NOT_FOUND, from_io};
use crate::table::{Table, lock};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, PoisonError, RwLock};

/// One mounted name.
pub(crate) struct Mount {
    pub(crate) name: Vec<u8>,
    pub(crate) root: PathBuf,
    pub(crate) ro: bool,
    /// The save transaction of a save mount, which the mounts of one save folder share.
    pub(crate) txn: Option<Arc<Txn>>,
    pub(crate) cache: IndexCache,
}

/// The state of the fs service.
pub(crate) struct Fs {
    pub(crate) cfg: Config,
    mounts: RwLock<Arc<Vec<Arc<Mount>>>>,
    mount_gate: Mutex<()>,
    pub(crate) files: Table<File>,
    pub(crate) dirs: Table<Dir>,
    pub(crate) commits: AtomicU64,
}

/// `<path><suffix>`, for the folders next to a save folder.
pub(crate) fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = OsString::from(p.as_os_str());
    s.push(suffix);
    PathBuf::from(s)
}

impl Fs {
    /// The service for `cfg`. Nothing is mounted.
    pub(crate) fn new(cfg: Config) -> Fs {
        Fs {
            cfg,
            mounts: RwLock::new(Arc::new(Vec::new())),
            mount_gate: Mutex::new(()),
            files: Table::new(),
            dirs: Table::new(),
            commits: AtomicU64::new(0),
        }
    }

    fn snapshot(&self) -> Arc<Vec<Arc<Mount>>> {
        Arc::clone(&self.mounts.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// The mount called `name`.
    pub(crate) fn find(&self, name: &[u8]) -> Option<Arc<Mount>> {
        self.snapshot().iter().find(|m| m.name == name).cloned()
    }

    /// Every mount, for the commit of a list of names and for the tests.
    pub(crate) fn all(&self) -> Arc<Vec<Arc<Mount>>> {
        self.snapshot()
    }

    fn publish(&self, m: Mount) {
        let mut w = self.mounts.write().unwrap_or_else(PoisonError::into_inner);
        let mut next: Vec<Arc<Mount>> = w.iter().cloned().collect();
        next.push(Arc::new(m));
        *w = Arc::new(next);
    }

    /// Mounts `root` as `name` unless `name` is mounted. The result is success in both cases.
    fn mount_with(&self, name: &[u8], make: impl FnOnce() -> FsResult<Mount>) -> u32 {
        let _gate = lock(&self.mount_gate);
        if self.find(name).is_some() {
            return OK;
        }
        match make() {
            Ok(m) => {
                self.publish(m);
                OK
            }
            Err(r) => r,
        }
    }

    fn plain(name: &[u8], root: PathBuf, ro: bool) -> Mount {
        Mount {
            name: name.to_vec(),
            root,
            ro,
            txn: None,
            cache: IndexCache::new(),
        }
    }

    /// Mounts the romfs of the game, read-only.
    pub(crate) fn mount_rom(&self, name: &[u8]) -> u32 {
        self.mount_with(name, || {
            if !self.cfg.romfs.is_dir() {
                eprintln!(
                    "nxkit-svc-fs: the romfs folder {} does not exist, so rom: cannot be mounted",
                    self.cfg.romfs.display()
                );
                return Err(FAIL);
            }
            Ok(Self::plain(name, self.cfg.romfs.clone(), true))
        })
    }

    /// The folder of the add-on content `index`, if the player has it.
    pub(crate) fn aoc_dir(&self, index: i32) -> Option<PathBuf> {
        let dir = self.cfg.aoc.as_ref()?.join(index.to_string());
        dir.is_dir().then_some(dir)
    }

    /// Mounts the add-on content `index`, read-only. Content that is not there gives "target not found".
    pub(crate) fn mount_aoc(&self, name: &[u8], index: i32) -> u32 {
        self.mount_with(name, || match self.aoc_dir(index) {
            Some(dir) => Ok(Self::plain(name, dir, true)),
            None => Err(TARGET_NOT_FOUND),
        })
    }

    /// Mounts the temporary storage, writable. The folder is created.
    pub(crate) fn mount_temp(&self, name: &[u8]) -> u32 {
        self.mount_with(name, || {
            std::fs::create_dir_all(&self.cfg.temp).map_err(|e| from_io(&e))?;
            Ok(Self::plain(name, self.cfg.temp.clone(), false))
        })
    }

    /// The folder of the cache storage `index`, a sibling of the temporary folder.
    pub(crate) fn cache_dir(&self, index: i32) -> PathBuf {
        with_suffix(&self.cfg.temp, &format!("_cache{index}"))
    }

    /// Creates the cache storage `index`.
    pub(crate) fn create_cache(&self, index: i32) -> u32 {
        match std::fs::create_dir_all(self.cache_dir(index)) {
            Ok(()) => OK,
            Err(e) => from_io(&e),
        }
    }

    /// Deletes the cache storage `index` and what it holds.
    pub(crate) fn delete_cache(&self, index: i32) -> u32 {
        match std::fs::remove_dir_all(self.cache_dir(index)) {
            Ok(()) => OK,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => OK,
            Err(e) => from_io(&e),
        }
    }

    /// Mounts the cache storage `index`, writable. The folder is created.
    pub(crate) fn mount_cache(&self, name: &[u8], index: i32) -> u32 {
        self.mount_with(name, || {
            let dir = self.cache_dir(index);
            std::fs::create_dir_all(&dir).map_err(|e| from_io(&e))?;
            Ok(Self::plain(name, dir, false))
        })
    }

    /// Mounts the save folder `dir` as `name`, with the transaction of the layout in R13. When the file system cannot
    /// clone folders the save is mounted read-only, and the log says so.
    pub(crate) fn mount_save(&self, name: &[u8], dir: &Path) -> u32 {
        self.mount_with(name, || {
            let shared = self.snapshot().iter().find_map(|m| m.txn.as_ref().filter(|t| t.commit == dir).cloned());
            let txn = match shared {
                Some(t) => t,
                None => match Txn::open(dir) {
                    Ok(t) => Arc::new(t),
                    Err(e) => {
                        let why = if crate::sys::is_unsupported(&e) {
                            "the volume cannot clone folders (it is not APFS)"
                        } else {
                            "the clone of the save folder failed"
                        };
                        eprintln!("[RED] nxkit-svc-fs: {why}: {e}. {} is mounted read-only, so no save is written", dir.display());
                        return Ok(Self::plain(name, dir.to_path_buf(), true));
                    }
                },
            };
            Ok(Mount {
                name: name.to_vec(),
                root: txn.work.clone(),
                ro: false,
                txn: Some(txn),
                cache: IndexCache::new(),
            })
        })
    }

    /// The account save folder: `Config.save` without trailing slashes.
    pub(crate) fn save_dir(&self) -> PathBuf {
        let s = self.cfg.save.to_string_lossy();
        let trimmed = s.trim_end_matches('/');
        if trimmed.is_empty() {
            self.cfg.save.clone()
        } else {
            PathBuf::from(trimmed)
        }
    }

    /// The device save folder, next to the account save folder.
    pub(crate) fn device_save_dir(&self) -> PathBuf {
        with_suffix(&self.save_dir(), "_device")
    }

    /// Splits a guest path into its mount and the components below the mount.
    pub(crate) fn locate(&self, path: &[u8]) -> FsResult<(Arc<Mount>, Vec<Vec<u8>>)> {
        let path = names::fwd_slashes(path);
        let (name, rest) = names::split_mount(&path).ok_or(PATH_NOT_FOUND)?;
        let mount = self.find(name).ok_or(PATH_NOT_FOUND)?;
        let comps = names::components(rest).ok_or(PATH_NOT_FOUND)?;
        Ok((mount, comps.into_iter().map(<[u8]>::to_vec).collect()))
    }

    /// The host path of `comps` in `mount`. With `create`, the last component may be new.
    pub(crate) fn host_path(mount: &Mount, comps: &[Vec<u8>], create: bool) -> FsResult<PathBuf> {
        let parts: Vec<&[u8]> = comps.iter().map(Vec::as_slice).collect();
        names::walk(&mount.cache, mount.ro, &mount.root, &parts, create).map_err(|e| from_io(&e))
    }

    /// The host path of an existing entry of the guest path `path`.
    pub(crate) fn existing(&self, path: &[u8]) -> FsResult<(Arc<Mount>, PathBuf)> {
        let (mount, comps) = self.locate(path)?;
        let host = Self::host_path(&mount, &comps, false)?;
        Ok((mount, host))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::config;

    #[test]
    fn a_name_that_is_mounted_can_be_mounted_again() {
        let fs = Fs::new(config("mount-twice"));
        assert_eq!(fs.mount_rom(b"rom"), OK);
        assert_eq!(fs.mount_rom(b"rom"), OK);
        assert_eq!(fs.all().len(), 1);
        assert!(fs.find(b"rom").is_some_and(|m| m.ro));
        assert!(fs.find(b"other").is_none());
    }

    #[test]
    fn add_on_content_that_is_not_there_is_not_found() {
        let cfg = config("mount-aoc");
        std::fs::create_dir_all(cfg.aoc.as_ref().unwrap().join("1")).unwrap();
        let fs = Fs::new(cfg);
        assert_eq!(fs.mount_aoc(b"aoc1", 1), OK);
        assert_eq!(fs.mount_aoc(b"aoc2", 2), TARGET_NOT_FOUND);
    }

    #[test]
    fn a_missing_romfs_fails_the_mount() {
        let mut cfg = config("mount-noromfs");
        cfg.romfs = cfg.romfs.join("missing");
        assert_eq!(Fs::new(cfg).mount_rom(b"rom"), FAIL);
    }

    #[test]
    fn temp_and_cache_are_created_and_writable() {
        let fs = Fs::new(config("mount-temp"));
        assert_eq!(fs.mount_temp(b"tmp"), OK);
        assert_eq!(fs.mount_cache(b"cache", 3), OK);
        assert!(fs.cfg.temp.is_dir());
        assert!(fs.cache_dir(3).is_dir());
        assert!(fs.find(b"cache").is_some_and(|m| !m.ro));
        assert_eq!(fs.delete_cache(3), OK);
        assert!(!fs.cache_dir(3).exists());
    }

    #[test]
    fn the_save_folder_has_no_trailing_slash() {
        let mut cfg = config("mount-savedir");
        cfg.save = PathBuf::from(format!("{}/", cfg.save.display()));
        let fs = Fs::new(cfg);
        assert!(!fs.save_dir().to_string_lossy().ends_with('/'));
        assert!(fs.device_save_dir().to_string_lossy().ends_with("_device"));
    }

    #[test]
    fn a_path_finds_its_mount() {
        let fs = Fs::new(config("mount-locate"));
        assert_eq!(fs.mount_temp(b"tmp"), OK);
        let (m, comps) = fs.locate(b"tmp:/a\\b/./c").unwrap();
        assert_eq!(m.name, b"tmp");
        assert_eq!(comps, vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]);
        assert_eq!(fs.locate(b"nope:/a").err(), Some(PATH_NOT_FOUND));
        assert_eq!(fs.locate(b"no colon").err(), Some(PATH_NOT_FOUND));
    }
}
