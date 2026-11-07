//! The save layout and the commit (R13).
//!
//! The committed save is the folder `<save>`. At mount, the host clones it to `<save>.work`, and the game reads and writes
//! the work folder. `CommitSaveData` clones the work folder to `<save>.new` and swaps it with `<save>` in one system
//! call, then removes the old version that is now in `<save>.new`. A kill at any time leaves the old or the new
//! committed save. The next mount removes what a kill left behind. This is what `dm/host/hle_fs.c` does, and the files on
//! disk are the same.
//!
//! A write that fails leaves the work folder in a state the game did not intend, so it poisons the transaction. The next
//! commit then drops the work folder, clones the committed save again and refuses, so a half-written save is never
//! committed.

use crate::mount::{Fs, with_suffix};
use crate::result::{FAIL, OK};
use crate::sys;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard};

/// The transaction of one save folder.
pub(crate) struct Txn {
    pub(crate) commit: PathBuf,
    pub(crate) work: PathBuf,
    neu: PathBuf,
    /// Writers hold the read side, a commit holds the write side, so a commit sees no write in flight.
    gate: RwLock<()>,
    poisoned: AtomicBool,
}

fn remove_tree(p: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(p) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

impl Txn {
    /// Opens the save folder `dir`: creates it, removes what a kill left behind and clones it to the work folder.
    pub(crate) fn open(dir: &Path) -> io::Result<Txn> {
        std::fs::create_dir_all(dir)?;
        let work = with_suffix(dir, ".work");
        let neu = with_suffix(dir, ".new");
        remove_tree(&work)?;
        remove_tree(&neu)?;
        sys::clone_dir(dir, &work)?;
        Ok(Txn { commit: dir.to_path_buf(), work, neu, gate: RwLock::new(()), poisoned: AtomicBool::new(false) })
    }

    /// Held by an operation that changes the work folder.
    pub(crate) fn writing(&self) -> RwLockReadGuard<'_, ()> {
        self.gate.read().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Marks the work folder as one that must not be committed.
    pub(crate) fn poison(&self) {
        self.poisoned.store(true, Ordering::SeqCst);
    }

    /// Whether the work folder is marked.
    pub(crate) fn is_poisoned(&self) -> bool {
        self.poisoned.load(Ordering::SeqCst)
    }

    /// Makes the work folder the committed save, or refuses.
    pub(crate) fn commit(&self) -> u32 {
        let _exclusive = self.gate.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.is_poisoned() {
            self.reset_work();
            eprintln!("[RED] nxkit-svc-fs: a write to {} failed, so the commit is refused and the work folder is reset", self.commit.display());
            return FAIL;
        }
        match self.publish() {
            Ok(()) => OK,
            Err(e) => {
                eprintln!("nxkit-svc-fs: the commit of {} failed: {e}", self.commit.display());
                FAIL
            }
        }
    }

    fn publish(&self) -> io::Result<()> {
        remove_tree(&self.neu)?;
        sys::clone_dir(&self.work, &self.neu)?;
        sys::swap(&self.neu, &self.commit)?;
        remove_tree(&self.neu)
    }

    /// Replaces the work folder by a clone of the committed save. The mark is removed only when that worked.
    fn reset_work(&self) {
        let re = with_suffix(&self.work, ".re");
        let done = remove_tree(&re)
            .and_then(|()| sys::clone_dir(&self.commit, &re))
            .and_then(|()| sys::swap(&re, &self.work))
            .and_then(|()| remove_tree(&re));
        match done {
            Ok(()) => self.poisoned.store(false, Ordering::SeqCst),
            Err(e) => eprintln!("nxkit-svc-fs: the reset of {} failed: {e}", self.work.display()),
        }
    }
}

impl Fs {
    /// `Commit(name)`. A name that is not mounted or has no save behind it succeeds, as in nxnative.
    pub(crate) fn commit(&self, name: &[u8]) -> u32 {
        let Some(txn) = self.find(name).and_then(|m| m.txn.clone()) else { return OK };
        self.commit_txn(&txn)
    }

    fn commit_txn(&self, txn: &Txn) -> u32 {
        let r = txn.commit();
        if r == OK {
            self.commits.fetch_add(1, Ordering::Relaxed);
        }
        r
    }

    /// `Commit(list)`. A name that is not mounted or is read-only fails. Names without a save are skipped. The names
    /// that have a save must share one, because the commits are not atomic together.
    pub(crate) fn commit_list(&self, names: &[Vec<u8>]) -> u32 {
        let mut txn: Option<Arc<Txn>> = None;
        for n in names {
            let Some(m) = self.find(n) else { return FAIL };
            if m.ro {
                return FAIL;
            }
            let Some(t) = &m.txn else { continue };
            match &txn {
                Some(first) if !Arc::ptr_eq(first, t) => return FAIL,
                Some(_) => {}
                None => txn = Some(Arc::clone(t)),
            }
        }
        match txn {
            Some(t) => self.commit_txn(&t),
            None => OK,
        }
    }

    /// The number of commits that succeeded, for the host's statistics.
    pub(crate) fn commit_count(&self) -> u64 {
        self.commits.load(Ordering::Relaxed)
    }
}

#[cfg(all(test, target_vendor = "apple"))]
mod tests {
    use super::*;
    use crate::testutil::config;

    fn saved(tag: &str) -> (Fs, Arc<Txn>) {
        let fs = Fs::new(config(tag));
        assert_eq!(fs.mount_save(b"save", &fs.save_dir()), OK);
        let txn = fs.find(b"save").and_then(|m| m.txn.clone()).unwrap();
        (fs, txn)
    }

    #[test]
    fn a_write_reaches_the_committed_save_only_at_commit() {
        let (fs, txn) = saved("commit-basic");
        std::fs::write(txn.work.join("option.sav"), b"one").unwrap();
        assert!(!txn.commit.join("option.sav").exists());
        assert_eq!(fs.commit(b"save"), OK);
        assert_eq!(std::fs::read(txn.commit.join("option.sav")).unwrap(), b"one");
        assert!(!with_suffix(&txn.commit, ".new").exists());
        assert_eq!(fs.commit_count(), 1);
    }

    #[test]
    fn the_next_mount_drops_what_was_not_committed() {
        let (fs, txn) = saved("commit-drop");
        std::fs::write(txn.work.join("lost.sav"), b"x").unwrap();
        let again = Fs::new(fs.cfg.clone());
        assert_eq!(again.mount_save(b"save", &again.save_dir()), OK);
        let work = again.find(b"save").unwrap().root.clone();
        assert!(!work.join("lost.sav").exists());
    }

    #[test]
    fn a_poisoned_save_is_reset_and_refused() {
        let (fs, txn) = saved("commit-poison");
        std::fs::write(txn.work.join("half.sav"), b"x").unwrap();
        txn.poison();
        assert_eq!(fs.commit(b"save"), FAIL);
        assert!(!txn.commit.join("half.sav").exists());
        assert!(!txn.work.join("half.sav").exists());
        assert!(!txn.is_poisoned());
        assert_eq!(fs.commit_count(), 0);
        assert_eq!(fs.commit(b"save"), OK);
    }

    #[test]
    fn commit_of_names_that_are_not_a_save() {
        let (fs, _txn) = saved("commit-names");
        assert_eq!(fs.mount_temp(b"tmp"), OK);
        assert_eq!(fs.commit(b"unknown"), OK);
        assert_eq!(fs.commit(b"tmp"), OK);
        assert_eq!(fs.commit_list(&[b"unknown".to_vec()]), FAIL);
        assert_eq!(fs.commit_list(&[b"tmp".to_vec(), b"save".to_vec()]), OK);
        assert_eq!(fs.commit_list(&[b"tmp".to_vec()]), OK);
    }

    #[test]
    fn two_names_for_one_save_share_the_work_folder() {
        let (fs, txn) = saved("commit-shared");
        assert_eq!(fs.mount_save(b"save2", &fs.save_dir()), OK);
        let second = fs.find(b"save2").unwrap();
        assert!(Arc::ptr_eq(second.txn.as_ref().unwrap(), &txn));
    }
}
