//! Where the save and the temporary storage of a game live (R67).
//!
//! The save is outside the app: `~/Library/Application Support/nxkit/<title id>/save`, or the folder that
//! `NXKIT_SAVE` names. The folder is the committed save of the transaction in `nxkit-svc-fs`, as `DM_SAVE` of the old
//! engine is. Its siblings are `<save>.work`, `<save>.new`, the device save `<save>_device` and the other users'
//! saves. The temporary storage is `~/Library/Application Support/nxkit/<title id>/temp`, and the cache storages are
//! its siblings.
//!
//! An app of an older build kept the save in its run folder, `save/<title id>`. [`migrate`] moves that save to the new
//! place once, at the first start that finds no save there.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

/// The folders of one game.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Places {
    /// The committed save of user 1. The other users' saves and the sync files are its siblings.
    pub save: PathBuf,
    /// The temporary storage. The cache storages are its siblings.
    pub temp: PathBuf,
    /// The save in the run folder that an older build wrote.
    pub bundled: PathBuf,
}

/// The title ID as the folder names have it: 16 hex digits in capitals, as the old engine names its folders.
pub fn title_folder(title_id: u64) -> String {
    format!("{title_id:016X}")
}

/// The folders of the game `title_id` with the run folder `root`. `title_name` is the `title_id` of `game.json` as it is
/// written, which names the old save in the run folder. `chosen` is `NXKIT_SAVE`. Without a home folder the save and
/// the temporary storage stay in the run folder, where an older build had them.
pub fn places(
    home: Option<&Path>,
    chosen: Option<&Path>,
    root: &Path,
    title_id: u64,
    title_name: &str,
) -> Places {
    let bundled = root.join("save").join(title_name);
    let (save, temp) = match home {
        Some(h) => {
            let support = h
                .join("Library/Application Support/nxkit")
                .join(title_folder(title_id));
            (support.join("save"), support.join("temp"))
        }
        None => (bundled.clone(), root.join("temp")),
    };
    Places {
        save: chosen.map_or(save, Path::to_path_buf),
        temp,
        bundled,
    }
}

/// `<path><suffix>`, for the folders next to a save folder.
pub fn sibling(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

/// Files that macOS writes into any folder and that are not part of a save.
fn not_save(name: &OsStr) -> bool {
    let n = name.as_encoded_bytes();
    n.starts_with(b"._") || n == b".DS_Store"
}

fn has_entries(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut d| d.any(|e| e.is_ok_and(|e| !not_save(&e.file_name()))))
}

/// Whether the save folder `save` or its device save holds files.
pub fn has_save(save: &Path) -> bool {
    has_entries(save) || has_entries(&sibling(save, "_device"))
}

/// What [`migrate`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Migration {
    /// There was no old save.
    Nothing,
    /// The old save is now at the new place.
    Moved,
    /// The old save stays where it is. The text says why.
    Kept(String),
}

/// Moves the old save `old` and its device save to `new`, when `old` has files and `new` has none. A rename moves a
/// folder on the same volume. On another volume the folders are copied with their attributes and the old ones are
/// removed after both copies are complete. An existing save is never replaced. When a step fails, what was moved goes
/// back and the old save stays.
pub fn migrate(old: &Path, new: &Path) -> Migration {
    if old == new || !has_save(old) {
        return Migration::Nothing;
    }
    if has_save(new) {
        return Migration::Kept(format!("the save at {} exists", new.display()));
    }
    let pairs: Vec<(PathBuf, PathBuf)> = [("", ""), ("_device", "_device")]
        .iter()
        .map(|(a, b)| (sibling(old, a), sibling(new, b)))
        .filter(|(from, _)| from.is_dir())
        .collect();
    match move_all(&pairs) {
        Ok(()) => {
            for stale in [".work", ".new", "_device.work", "_device.new"] {
                let _ = std::fs::remove_dir_all(sibling(old, stale));
            }
            Migration::Moved
        }
        Err(e) => Migration::Kept(e.to_string()),
    }
}

/// Moves each `(from, to)`. An empty folder at `to` is removed first. On a failure the folders that were moved go back.
fn move_all(pairs: &[(PathBuf, PathBuf)]) -> io::Result<()> {
    if let Some(parent) = pairs.first().and_then(|(_, to)| to.parent()) {
        std::fs::create_dir_all(parent)?;
    }
    for (_, to) in pairs {
        if to.exists() {
            std::fs::remove_dir(to)?;
        }
    }
    let mut renamed: Vec<&(PathBuf, PathBuf)> = Vec::new();
    for pair in pairs {
        match std::fs::rename(&pair.0, &pair.1) {
            Ok(()) => renamed.push(pair),
            Err(e) if e.raw_os_error() == Some(EXDEV) => {
                for (from, to) in renamed {
                    std::fs::rename(to, from)?;
                }
                return copy_all(pairs);
            }
            Err(e) => {
                for (from, to) in renamed {
                    std::fs::rename(to, from)?;
                }
                return Err(e);
            }
        }
    }
    Ok(())
}

/// The error of a rename to another volume.
const EXDEV: i32 = 18;

/// Copies each `(from, to)`, then removes the sources. A failed copy removes the copies.
fn copy_all(pairs: &[(PathBuf, PathBuf)]) -> io::Result<()> {
    for (i, (from, to)) in pairs.iter().enumerate() {
        if let Err(e) = nxb_sys::clone::clone_or_copy(from, to) {
            for (_, done) in &pairs[..=i] {
                let _ = std::fs::remove_dir_all(done);
            }
            return Err(e);
        }
    }
    for (from, _) in pairs {
        std::fs::remove_dir_all(from)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nxkit-saveplace-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_save_is_under_application_support_unless_chosen() {
        let p = places(
            Some(Path::new("/Users/a")),
            None,
            Path::new("/App/nxkit"),
            0x0100_abcd_0000_0000,
            "0100abcd00000000",
        );
        let support = "/Users/a/Library/Application Support/nxkit/0100ABCD00000000";
        assert_eq!(p.save, Path::new(support).join("save"));
        assert_eq!(p.temp, Path::new(support).join("temp"));
        assert_eq!(p.bundled, Path::new("/App/nxkit/save/0100abcd00000000"));
        let chosen = places(
            Some(Path::new("/Users/a")),
            Some(Path::new("/x/s")),
            Path::new("/r"),
            1,
            "1",
        );
        assert_eq!(chosen.save, Path::new("/x/s"));
        let homeless = places(None, None, Path::new("/r"), 1, "1");
        assert_eq!(
            (homeless.save, homeless.temp),
            (PathBuf::from("/r/save/1"), PathBuf::from("/r/temp"))
        );
    }

    #[test]
    fn an_old_save_moves_once_with_its_device_save() {
        let d = scratch("move");
        let (old, new) = (d.join("bundle/save/1"), d.join("support/1/save"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("a.sav"), b"1").unwrap();
        std::fs::create_dir_all(sibling(&old, "_device")).unwrap();
        std::fs::write(sibling(&old, "_device").join("d.sav"), b"2").unwrap();
        std::fs::create_dir_all(sibling(&old, ".work")).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        assert_eq!(migrate(&old, &new), Migration::Moved);
        assert_eq!(std::fs::read(new.join("a.sav")).unwrap(), b"1");
        assert_eq!(
            std::fs::read(sibling(&new, "_device").join("d.sav")).unwrap(),
            b"2"
        );
        assert!(!old.exists() && !sibling(&old, ".work").exists());
        assert_eq!(migrate(&old, &new), Migration::Nothing);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn an_existing_save_is_never_replaced() {
        let d = scratch("keep");
        let (old, new) = (d.join("old"), d.join("new"));
        for (dir, v) in [(&old, b"old"), (&new, b"new")] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("a.sav"), v).unwrap();
        }
        assert!(matches!(migrate(&old, &new), Migration::Kept(_)));
        assert_eq!(std::fs::read(new.join("a.sav")).unwrap(), b"new");
        assert_eq!(std::fs::read(old.join("a.sav")).unwrap(), b"old");
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_failed_move_leaves_the_old_save() {
        let d = scratch("back");
        let (old, new) = (d.join("old"), d.join("new"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("a.sav"), b"1").unwrap();
        std::fs::create_dir_all(sibling(&old, "_device")).unwrap();
        std::fs::write(sibling(&old, "_device").join(".DS_Store"), b"").unwrap();
        // A file where the device save goes: the move stops before anything moved.
        std::fs::write(sibling(&new, "_device"), b"x").unwrap();
        assert!(matches!(migrate(&old, &new), Migration::Kept(_)));
        assert_eq!(std::fs::read(old.join("a.sav")).unwrap(), b"1");
        assert!(!new.exists());
        std::fs::remove_dir_all(d).unwrap();
    }
}
