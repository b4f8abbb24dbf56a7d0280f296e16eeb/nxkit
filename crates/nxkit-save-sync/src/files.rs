//! Folder helpers of the save layout: the names of a folder in byte order, the two folders of a save, removal.

use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// `<path><suffix>`, for the files and folders next to a save.
pub(crate) fn sibling(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

/// Folder `i` of a save: 0 the save, 1 the device save `<save>_device`.
pub(crate) fn folder(save: &Path, i: usize) -> PathBuf {
    if i == 0 {
        save.to_path_buf()
    } else {
        sibling(save, "_device")
    }
}

/// The names in a folder in byte order. A read that fails partway is a failure, not the end of the folder.
pub(crate) fn names(dir: &Path) -> io::Result<Vec<OsString>> {
    let mut v = std::fs::read_dir(dir)?
        .map(|e| e.map(|e| e.file_name()))
        .collect::<io::Result<Vec<_>>>()?;
    v.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    Ok(v)
}

/// Whether a folder holds anything but Finder's `.DS_Store`: an empty folder is no save. A missing folder is none. A
/// folder that cannot be read is an error, never "no save": a send that took it for none would replace a cloud save
/// that has it.
pub(crate) fn has_entries(dir: &Path) -> io::Result<bool> {
    match names(dir) {
        Ok(v) => Ok(v.iter().any(|n| n.as_bytes() != b".DS_Store")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Removes `p` and what it holds. Nothing there is success.
pub(crate) fn rm_tree(p: &Path) -> io::Result<()> {
    let md = match std::fs::symlink_metadata(p) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if md.is_dir() {
        std::fs::remove_dir_all(p)
    } else {
        std::fs::remove_file(p)
    }
}

/// Whether `p` exists, a link counted as itself.
pub(crate) fn exists(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}
