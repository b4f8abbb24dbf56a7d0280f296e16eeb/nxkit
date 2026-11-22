//! The store itself: blobs, entries, lookup, verify, restore, quarantine.

use crate::dirty::{self, Journal};
use crate::error::StoreError;
use crate::fsutil::{
    commit_rename, ensure_dir, temp_path, write_durable, write_temp_hashed, write_temp_plain,
};
use crate::manifest::{Manifest, validate_name};
use nxb_model::{ContentKey, Digest, FileDigest, Sha256};
use nxb_sys::lock::{FileLock, LockMode};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const FORMAT: &str = "nxb-store 1\n";

/// A store rooted at a directory. Cheap to share across threads (`&Store` is enough for every operation).
#[derive(Debug)]
pub struct Store {
    root: PathBuf,
    durability: Durability,
    quarantined: AtomicU64,
    recovered: u64,
    journal: Journal,
    exclusive: bool,
    _root_lock: FileLock,
}

/// How hard the store pushes writes to the disk.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Durability {
    /// `fsync` every file before its rename and the folder after it (survives power loss). Default.
    #[default]
    Full,
    /// Temp name and rename only, no `fsync`: survives a crash of the process (the journal names what to check), not
    /// necessarily a power loss. Lookups still check sizes and drift sampling re-hashes. Measured: four `fsync`s per
    /// entry cost about 230 ms on the external USB volume under load (P1a run).
    Relaxed,
}

/// The result of [`Store::verify`].
#[derive(Debug, PartialEq, Eq)]
pub enum Verified {
    /// No entry for the key.
    Missing,
    /// Every blob has its recorded size and digest.
    Intact(Manifest),
    /// Something was damaged and moved to quarantine, the entry is gone.
    Quarantined(String),
}

fn shard(hex: &str) -> &str {
    hex.get(..2).unwrap_or("00")
}

impl Store {
    /// Open (or create) a store at `root` for a build: a shared lock on the root (waits while a prune or migration
    /// holds it exclusively), then recovery of journals left by dead writers.
    pub fn open(root: &Path) -> Result<Store, StoreError> {
        Self::open_with(root, LockMode::Shared, true)
    }

    /// Open with the root locked exclusively (prune, compaction, migration). With `wait` false a busy root is
    /// [`StoreError::Locked`].
    pub fn open_exclusive(root: &Path, wait: bool) -> Result<Store, StoreError> {
        Self::open_with(root, LockMode::Exclusive, wait)
    }

    fn open_with(root: &Path, mode: LockMode, wait: bool) -> Result<Store, StoreError> {
        for d in ["blobs", "entries", "quarantine", "tmp", "dirty"] {
            ensure_dir(&root.join(d))?;
        }
        let lock_path = root.join("LOCK");
        let root_lock = FileLock::acquire(&lock_path, mode, wait)
            .map_err(|e| StoreError::io("lock store", &lock_path, e))?
            .ok_or_else(|| StoreError::Locked(root.to_path_buf()))?;
        let fmt_path = root.join("FORMAT");
        match fs::read_to_string(&fmt_path) {
            Ok(s) if s == FORMAT => {}
            Ok(s) => return Err(StoreError::WrongFormat(s.chars().take(40).collect())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                write_durable(&root.join("tmp"), &fmt_path, FORMAT.as_bytes(), true)?;
            }
            Err(e) => return Err(StoreError::io("read format", fmt_path, e)),
        }
        let mut store = Store {
            root: root.to_path_buf(),
            durability: Durability::Full,
            quarantined: AtomicU64::new(0),
            recovered: 0,
            journal: Journal::create(&root.join("dirty"))?,
            exclusive: mode == LockMode::Exclusive,
            _root_lock: root_lock,
        };
        store.recover()?;
        Ok(store)
    }

    /// Check what dead writers were writing (their journals), quarantine damage, remove their temp files.
    fn recover(&mut self) -> Result<(), StoreError> {
        for o in dirty::orphans(&self.root.join("dirty"))? {
            for hex in &o.blobs {
                let Ok(d) = Digest::from_hex(hex) else {
                    continue;
                };
                let path = self.blob_path(&d);
                if path.exists() && hash_file(&path)?.sha256 != d {
                    self.quarantine_blob(&d, "damaged after an unclean end")?;
                }
            }
            for hex in &o.entries {
                if let Ok(k) = ContentKey::from_hex(hex) {
                    self.verify(&k)?;
                }
            }
            dirty::finish(&self.tmp(), o)?;
            self.recovered += 1;
        }
        Ok(())
    }

    /// Journals of dead writers checked when this handle opened (a metric, normally 0).
    pub fn recovered(&self) -> u64 {
        self.recovered
    }

    /// Change the durability of later writes.
    pub fn set_durability(&mut self, d: Durability) {
        self.durability = d;
    }

    fn sync(&self) -> bool {
        self.durability == Durability::Full
    }

    /// True when this handle holds the root exclusively (prune needs it).
    pub fn is_exclusive(&self) -> bool {
        self.exclusive
    }

    /// The root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Items moved to quarantine by this handle (a metric).
    pub fn quarantined(&self) -> u64 {
        self.quarantined.load(Ordering::Relaxed)
    }

    fn tmp(&self) -> PathBuf {
        self.root.join("tmp")
    }

    /// Where a blob lives.
    pub fn blob_path(&self, d: &Digest) -> PathBuf {
        let hex = d.to_hex();
        self.root.join("blobs").join(shard(&hex)).join(hex)
    }

    pub(crate) fn entry_path(&self, key: &ContentKey) -> PathBuf {
        let hex = key.to_hex();
        self.root
            .join("entries")
            .join(shard(&hex))
            .join(format!("{hex}.manifest"))
    }

    fn blob_has_size(&self, d: &FileDigest) -> bool {
        fs::metadata(self.blob_path(&d.sha256)).is_ok_and(|m| m.is_file() && m.len() == d.len)
    }

    /// Store bytes held in memory, they are hashed once, here. Writing a blob that exists is a no-op.
    pub fn put_bytes(&self, bytes: &[u8]) -> Result<FileDigest, StoreError> {
        let digest = FileDigest::of(bytes);
        if self.blob_has_size(&digest) {
            return Ok(digest);
        }
        self.journal.note("blob", &digest.sha256.to_hex())?;
        let tmp = write_temp_plain(&self.tmp(), bytes, self.sync())?;
        commit_rename(&tmp, &self.blob_path(&digest.sha256), self.sync())?;
        Ok(digest)
    }

    /// Store a stream, it is hashed while it is written, never read twice.
    pub fn put_reader(&self, src: impl Read) -> Result<FileDigest, StoreError> {
        let (tmp, digest) = write_temp_hashed(&self.tmp(), src, self.sync())?;
        if self.blob_has_size(&digest) {
            let _ = fs::remove_file(&tmp);
            return Ok(digest);
        }
        self.journal.note("blob", &digest.sha256.to_hex())?;
        commit_rename(&tmp, &self.blob_path(&digest.sha256), self.sync())?;
        Ok(digest)
    }

    /// Start an entry for `key`. Nothing is visible until [`EntryWriter::commit`].
    pub fn entry(&self, key: ContentKey) -> EntryWriter<'_> {
        EntryWriter {
            store: self,
            key,
            files: BTreeMap::new(),
        }
    }

    /// Find an entry. Checks the manifest checksum and each blob's size, damage is quarantined and reported as a miss.
    pub fn lookup(&self, key: &ContentKey) -> Result<Option<Manifest>, StoreError> {
        let path = self.entry_path(key);
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(StoreError::io("read manifest", path, e)),
        };
        let Some(m) = Manifest::parse(&bytes).filter(|m| m.key == *key) else {
            self.quarantine_entry(key, "manifest damaged")?;
            return Ok(None);
        };
        if let Some(f) = m.files.iter().find(|f| !self.blob_has_size(&f.digest)) {
            let why = format!("blob of {} missing or wrong size", f.name);
            self.quarantine_blob(&f.digest.sha256, &why)?;
            self.quarantine_entry(key, &why)?;
            return Ok(None);
        }
        Ok(Some(m))
    }

    /// Full check: [`Store::lookup`] plus a re-hash of every blob (after an unclean end, or drift sampling).
    pub fn verify(&self, key: &ContentKey) -> Result<Verified, StoreError> {
        let before = self.quarantined();
        let Some(m) = self.lookup(key)? else {
            return Ok(if self.quarantined() > before {
                Verified::Quarantined("damaged at lookup".into())
            } else {
                Verified::Missing
            });
        };
        for f in &m.files {
            let path = self.blob_path(&f.digest.sha256);
            let actual = hash_file(&path)?;
            if actual != f.digest {
                let why = format!("blob of {} has another digest", f.name);
                self.quarantine_blob(&f.digest.sha256, &why)?;
                self.quarantine_entry(key, &why)?;
                return Ok(Verified::Quarantined(why));
            }
        }
        Ok(Verified::Intact(m))
    }

    /// Read a blob into memory.
    pub fn read_blob(&self, d: &Digest) -> Result<Vec<u8>, StoreError> {
        let p = self.blob_path(d);
        fs::read(&p).map_err(|e| StoreError::io("read blob", p, e))
    }

    /// Materialise an entry's files under `dest`, each through a temp name and a rename (APFS clones where
    /// `std::fs::copy` can). Outputs are not fsynced one by one: the caller's dirty marker covers them (design-v2 10.3).
    pub fn restore(&self, m: &Manifest, dest: &Path) -> Result<(), StoreError> {
        for f in &m.files {
            validate_name(&f.name)?;
            let dst = dest.join(&f.name);
            let dir = dst.parent().unwrap_or(dest);
            ensure_dir(dir)?;
            let tmp = temp_path(dir);
            let src = self.blob_path(&f.digest.sha256);
            fs::copy(&src, &tmp).map_err(|e| StoreError::io("copy blob", &src, e))?;
            fs::rename(&tmp, &dst).map_err(|e| {
                let _ = fs::remove_file(&tmp);
                StoreError::io("rename output", &dst, e)
            })?;
        }
        Ok(())
    }

    fn quarantine_dir(&self, what: &str) -> Result<PathBuf, StoreError> {
        let n = self.quarantined.fetch_add(1, Ordering::Relaxed);
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = self
            .root
            .join("quarantine")
            .join(format!("{t}-{}-{n}-{what}", std::process::id()));
        ensure_dir(&dir)?;
        Ok(dir)
    }

    fn quarantine_move(&self, src: &Path, what: &str, why: &str) -> Result<(), StoreError> {
        if !src.exists() {
            return Ok(());
        }
        let dir = self.quarantine_dir(what)?;
        let name = src
            .file_name()
            .map(PathBuf::from)
            .unwrap_or_else(|| "item".into());
        match fs::rename(src, dir.join(name)) {
            Ok(()) => {}
            // Another thread or process moved it first.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(StoreError::io("quarantine", src, e)),
        }
        let _ = fs::write(dir.join("reason.txt"), format!("{why}\n"));
        Ok(())
    }

    fn quarantine_entry(&self, key: &ContentKey, why: &str) -> Result<(), StoreError> {
        self.quarantine_move(
            &self.entry_path(key),
            &format!("entry-{}", key.to_hex()),
            why,
        )
    }

    fn quarantine_blob(&self, d: &Digest, why: &str) -> Result<(), StoreError> {
        self.quarantine_move(&self.blob_path(d), &format!("blob-{}", d.to_hex()), why)
    }
}

fn hash_file(path: &Path) -> Result<FileDigest, StoreError> {
    let mut f = File::open(path).map_err(|e| StoreError::io("open blob", path, e))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => h.update(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(StoreError::io("read blob", path, e)),
        }
    }
    Ok(FileDigest {
        len: h.len(),
        sha256: h.finalize(),
    })
}

/// Collects an entry's files. [`EntryWriter::commit`] writes the manifest last.
#[derive(Debug)]
pub struct EntryWriter<'s> {
    store: &'s Store,
    key: ContentKey,
    files: BTreeMap<String, FileDigest>,
}

impl EntryWriter<'_> {
    /// Add a blob that is already in the store under `name` (its digest came from the producer).
    pub fn add(&mut self, name: &str, digest: FileDigest) -> Result<&mut Self, StoreError> {
        validate_name(name)?;
        if !self.store.blob_has_size(&digest) {
            return Err(StoreError::MissingBlob(digest.sha256));
        }
        if self.files.insert(name.to_string(), digest).is_some() {
            return Err(StoreError::BadName(format!("{name} (twice)")));
        }
        Ok(self)
    }

    /// Store bytes and add them under `name`.
    pub fn add_bytes(&mut self, name: &str, bytes: &[u8]) -> Result<FileDigest, StoreError> {
        validate_name(name)?;
        let d = self.store.put_bytes(bytes)?;
        self.add(name, d)?;
        Ok(d)
    }

    /// Write the manifest (temp, fsync, rename, directory fsync). Publishing the same key again replaces the entry
    /// atomically, with deterministic producers the content is the same.
    pub fn commit(self) -> Result<Manifest, StoreError> {
        let m = Manifest::new(self.key, self.files)?;
        self.store.journal.note("entry", &m.key.to_hex())?;
        write_durable(
            &self.store.tmp(),
            &self.store.entry_path(&m.key),
            &m.to_bytes(),
            self.store.sync(),
        )?;
        Ok(m)
    }
}

/// Lock a build's output directory exclusively (`<dir>/<lock_name>`, for example `.shaderbuild.lock`). With `wait`
/// false a held lock is [`StoreError::Locked`], which the stage reports as `[RED] another shader build is using <dir>`.
/// The lock is released when the returned guard drops or the process dies.
pub fn lock_output_dir(dir: &Path, lock_name: &str, wait: bool) -> Result<FileLock, StoreError> {
    validate_name(lock_name)?;
    ensure_dir(dir)?;
    let path = dir.join(lock_name);
    FileLock::acquire(&path, LockMode::Exclusive, wait)
        .map_err(|e| StoreError::io("lock output directory", &path, e))?
        .ok_or_else(|| StoreError::Locked(dir.to_path_buf()))
}
