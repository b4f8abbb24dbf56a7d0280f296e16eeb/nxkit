//! The pull: the cloud's save replaces this device's, with a journal that puts this device's save back after a failed
//! or killed pull. As `pull`, `recover` and the journal of `save_sync.c`, with the same journal file.
//!
//! The journal `<save>.pull` is written and synced before the first folder of this device moves, and removed once the
//! cloud's save is wholly in place. It names the backup folder and, for each folder, what this device had there:
//! nothing, an empty folder, or a save with the folder's device and inode. A rename or a swap moves a folder whole and
//! keeps its inode, so [`recover`] finds this device's folder wherever a pull stopped and puts it back.

use crate::files::{self, folder, sibling};
use crate::format::FOLDERS;
use crate::{Meta, red, state, sys, unpack};
use std::ffi::OsStr;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// What this device had in a folder before the pull.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Had {
    #[default]
    None = 0,
    Empty = 1,
    Save = 2,
}

/// The journal of a pull.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Journal {
    /// The backup folder of this device's save, empty when it had none.
    pub(crate) kept: PathBuf,
    pub(crate) had: [Had; 2],
    pub(crate) dev: [u64; 2],
    pub(crate) ino: [u64; 2],
}

/// The text of the journal.
pub(crate) fn journal_text(j: &Journal) -> Vec<u8> {
    let mut b = b"# save sync (dm/host/save_sync.c): a pull of the cloud's save in progress; the next start puts this \
device's save back\nkept\t"
        .to_vec();
    b.extend_from_slice(j.kept.as_os_str().as_bytes());
    b.push(b'\n');
    for i in 0..2 {
        b.extend_from_slice(
            format!(
                "folder\t{i}\t{}\t{}\t{}\n",
                j.had[i] as u8, j.dev[i], j.ino[i]
            )
            .as_bytes(),
        );
    }
    b
}

/// The journal of a text, or `None` when a line is missing.
pub(crate) fn parse_journal(text: &[u8]) -> Option<Journal> {
    let mut j = Journal::default();
    let mut got = 0u8;
    for line in text.split(|c| *c == b'\n') {
        if let Some(k) = line.strip_prefix(b"kept\t") {
            j.kept = PathBuf::from(OsStr::from_bytes(k));
            got |= 4;
            continue;
        }
        let Some(rest) = line.strip_prefix(b"folder\t") else {
            continue;
        };
        let f: Vec<u64> = std::str::from_utf8(rest)
            .ok()?
            .split('\t')
            .map_while(|w| w.parse().ok())
            .collect();
        if let [i @ 0..=1, had @ 0..=2, dev, ino] = f[..] {
            let i = i as usize;
            j.had[i] = [Had::None, Had::Empty, Had::Save][had as usize];
            (j.dev[i], j.ino[i]) = (dev, ino);
            got |= 1 << i;
        }
    }
    (got == 7).then_some(j)
}

fn journal_path(save: &Path) -> PathBuf {
    sibling(save, ".pull")
}

fn journal_write(save: &Path, j: &Journal) -> io::Result<()> {
    let p = journal_path(save);
    let t = sibling(&p, ".tmp");
    let r = (|| {
        let mut f = std::fs::File::create(&t)?;
        f.write_all(&journal_text(j))?;
        f.sync_all()?;
        std::fs::rename(&t, &p)
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&t);
    }
    if let Some(dir) = p.parent()
        && let Ok(d) = std::fs::File::open(dir)
    {
        let _ = d.sync_all();
    }
    r
}

/// The journal of a pull that did not finish, `None` when there is none, an error when it cannot be read.
pub(crate) fn journal_read(save: &Path) -> io::Result<Option<Journal>> {
    match std::fs::read(journal_path(save)) {
        Ok(t) => parse_journal(&t)
            .map(Some)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "the journal is incomplete")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn is_folder(p: &Path, dev: u64, ino: u64) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir() && m.dev() == dev && m.ino() == ino)
}

/// Puts this device's save back as it was before the pull of the journal, from wherever that pull stopped, then
/// removes the journal. What it removes is only ever the cloud's. An error keeps the journal.
pub(crate) fn recover(save: &Path, j: &Journal) -> io::Result<()> {
    let kept = !j.kept.as_os_str().is_empty();
    let mut first: Option<io::Error> = None;
    let mut fail = |e: io::Error| {
        first.get_or_insert(e);
    };
    for i in 0..2 {
        let here = folder(save, i);
        let old = j.kept.join(FOLDERS[i]);
        if j.had[i] == Had::Save {
            if is_folder(&here, j.dev[i], j.ino[i]) {
                if kept && files::exists(&old) {
                    files::rm_tree(&old).unwrap_or_else(&mut fail);
                }
            } else if kept && is_folder(&old, j.dev[i], j.ino[i]) {
                let back = if files::exists(&here) {
                    sys::swap(&old, &here).and_then(|()| files::rm_tree(&old))
                } else {
                    std::fs::rename(&old, &here)
                };
                back.unwrap_or_else(&mut fail);
            } else {
                red(&format!(
                    "{}: this device's folder is neither in place nor in {}",
                    here.display(),
                    if kept {
                        j.kept.display().to_string()
                    } else {
                        "the backups".into()
                    }
                ));
                fail(io::Error::from(io::ErrorKind::NotFound));
            }
        } else {
            match files::rm_tree(&here) {
                Ok(()) if j.had[i] == Had::Empty => {
                    std::fs::create_dir_all(&here).unwrap_or_else(&mut fail)
                }
                Ok(()) => {}
                Err(e) => fail(e),
            }
        }
    }
    if let Some(e) = first {
        return Err(e);
    }
    if kept {
        let _ = std::fs::remove_dir(&j.kept);
    }
    match std::fs::remove_file(journal_path(save)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// A pull that did not bring the cloud's save in.
#[derive(Debug)]
pub(crate) enum PullError {
    /// This device's save is as it was.
    Unchanged(io::Error),
    /// This device's save could not be put back: the journal stays, and the next start tries again. The path is where
    /// its folders are kept.
    Unsafe(io::Error, PathBuf),
}

/// The cloud's save `bundle` replaces this one. It is unpacked beside it (`<save>.sync-tmp/in`), the journal written,
/// then each folder of this device moves into the backup folder and is swapped from there with the cloud's. Gives the
/// cloud's save and the backup of this device's (empty when it had none).
pub(crate) fn pull(save: &Path, bundle: &Path) -> Result<(Meta, PathBuf), PullError> {
    let tmp = sibling(save, ".sync-tmp").join("in");
    let unchanged = |e: io::Error| {
        let _ = files::rm_tree(&tmp);
        PullError::Unchanged(e)
    };
    let meta = files::rm_tree(&tmp)
        .and_then(|()| std::fs::create_dir_all(&tmp))
        .and_then(|()| unpack::read(bundle, Some(&tmp)))
        .map_err(unchanged)?;
    let mut j = Journal::default();
    let mut fresh = [false; 2];
    for i in 0..2 {
        fresh[i] = files::exists(&tmp.join(FOLDERS[i]));
        let here = folder(save, i);
        if let Ok(md) = std::fs::symlink_metadata(&here) {
            if files::has_entries(&here).map_err(unchanged)? {
                j.had[i] = Had::Save;
                (j.dev[i], j.ino[i]) = (md.dev(), md.ino());
            } else {
                j.had[i] = Had::Empty;
            }
        }
    }
    if j.had.contains(&Had::Save) {
        j.kept = state::backup_dir(save, "local", false).map_err(unchanged)?;
    }
    if let Err(e) = journal_write(save, &j) {
        if !j.kept.as_os_str().is_empty() {
            let _ = std::fs::remove_dir(&j.kept);
        }
        return Err(unchanged(e));
    }
    let moved = (|| {
        for i in 0..2 {
            let (here, incoming, old) = (
                folder(save, i),
                tmp.join(FOLDERS[i]),
                j.kept.join(FOLDERS[i]),
            );
            match (j.had[i], fresh[i]) {
                (Had::Save, true) => {
                    std::fs::rename(&incoming, &old).and_then(|()| sys::swap(&old, &here))?
                }
                (Had::Save, false) => std::fs::rename(&here, &old)?,
                (_, true) if files::exists(&here) => sys::swap(&incoming, &here)?,
                (_, true) => std::fs::rename(&incoming, &here)?,
                (_, false) => {}
            }
        }
        std::fs::remove_file(journal_path(save))
    })();
    match moved {
        Ok(()) => {
            let _ = files::rm_tree(&tmp);
            if !j.kept.as_os_str().is_empty() {
                state::prune_backups(save, "local", &j.kept);
            }
            Ok((meta, j.kept))
        }
        Err(e) => {
            let back = recover(save, &j).is_ok();
            let _ = files::rm_tree(&tmp);
            if back {
                Err(PullError::Unchanged(e))
            } else {
                Err(PullError::Unsafe(e, j.kept))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_journal_has_the_lines_of_save_sync_c() {
        let j = Journal {
            kept: PathBuf::from("/s-backups/2026-11-21 10.00.00 local"),
            had: [Had::Save, Had::Empty],
            dev: [16777232, 0],
            ino: [42, 0],
        };
        let t = journal_text(&j);
        let s = String::from_utf8(t.clone()).unwrap();
        assert!(s.contains("\nkept\t/s-backups/2026-11-21 10.00.00 local\nfolder\t0\t2\t16777232\t42\nfolder\t1\t1\t0\t0\n"));
        assert_eq!(parse_journal(&t), Some(j));
        assert_eq!(parse_journal(b"kept\t\nfolder\t0\t0\t0\t0\n"), None);
        assert_eq!(
            parse_journal(b"kept\t\nfolder\t0\t0\t0\t0\nfolder\t1\t3\t0\t0\n"),
            None
        );
    }
}
