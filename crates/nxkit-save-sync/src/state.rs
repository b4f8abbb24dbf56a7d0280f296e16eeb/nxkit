//! The files next to a save: `<save>.sync` with the hash of the last sync, the backups in `<save>-backups`, and the
//! path of the cloud's file. As `save_sync.c`.

use crate::files::{self, sibling};
use crate::format::is_hash;
use crate::{log, red, sys};
use std::io;
use std::path::{Path, PathBuf};

/// The backups kept of each side (local, cloud). Older ones are removed when a new one is made.
pub(crate) const KEEP_BACKUPS: usize = 10;

/// The hash of the save last known to be the same here and in the cloud ("" for none), from `<save>.sync`.
pub(crate) fn read_base(save: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(sibling(save, ".sync")) else {
        return String::new();
    };
    let mut base = String::new();
    for line in text.lines() {
        if let Some(h) = line.strip_prefix("base\t")
            && h.len() >= 32
            && is_hash(&h[..32])
        {
            base = h[..32].to_owned();
        }
    }
    base
}

/// Writes `<save>.sync`: beside it first, then renamed over it, so a cut-off write never reads as another base.
pub(crate) fn write_base(save: &Path, hash: &str) {
    let p = sibling(save, ".sync");
    let t = sibling(&p, ".tmp");
    let text = format!(
        "# save sync (dm/host/save_sync.h): the save last known to be the same here and in the cloud\nbase\t{hash}\n"
    );
    if let Err(e) = std::fs::write(&t, text).and_then(|()| std::fs::rename(&t, &p)) {
        let _ = std::fs::remove_file(&t);
        red(&format!(
            "cannot write {} ({e}): the next start compares with the sync before",
            p.display()
        ));
    }
}

/// A time as the log and the backups show it: `YYYY-MM-DD HH<sep>MM<sep>SS` in local time, or `unknown`.
pub(crate) fn stamp(t: i64, sep: char) -> String {
    match sys::local_time(t) {
        Some([y, mo, d, h, mi, s]) if t != 0 => {
            format!("{y:04}-{mo:02}-{d:02} {h:02}{sep}{mi:02}{sep}{s:02}")
        }
        _ => "unknown".to_owned(),
    }
}

/// The time of a save in the log.
pub(crate) fn when(t: i64) -> String {
    stamp(t, ':')
}

/// Whether `name` is a backup of `side` that this crate made: `YYYY-MM-DD HH.MM.SS <side>`, with ` <k>` after it.
/// Anything else in `<save>-backups` is the player's.
pub(crate) fn is_backup(name: &str, side: &str) -> bool {
    const FORM: &[u8] = b"0000-00-00 00.00.00 ";
    let b = name.as_bytes();
    if b.len() < FORM.len()
        || !FORM.iter().zip(b).all(|(f, c)| {
            if *f == b'0' {
                c.is_ascii_digit()
            } else {
                f == c
            }
        })
    {
        return false;
    }
    let Some(rest) = name[FORM.len()..].strip_prefix(side) else {
        return false;
    };
    match rest.strip_prefix(' ') {
        None => rest.is_empty(),
        Some(k) => !k.is_empty() && k.bytes().all(|c| c.is_ascii_digit()),
    }
}

/// Removes the oldest backups of `side` so that the newest [`KEEP_BACKUPS`] stay. `made`, just made, is one of them
/// whatever its date says.
pub(crate) fn prune_backups(save: &Path, side: &str, made: &Path) {
    let dir = sibling(save, "-backups");
    let Ok(names) = files::names(&dir) else {
        return;
    };
    let mine = made.file_name();
    let old: Vec<_> = names
        .into_iter()
        .filter(|n| Some(n.as_os_str()) != mine && n.to_str().is_some_and(|s| is_backup(s, side)))
        .collect();
    let mut gone = 0;
    for n in old
        .iter()
        .take((old.len() + 1).saturating_sub(KEEP_BACKUPS))
    {
        let p = dir.join(n);
        match files::rm_tree(&p) {
            Ok(()) => gone += 1,
            Err(e) => red(&format!(
                "cannot remove the old backup {} ({e})",
                p.display()
            )),
        }
    }
    if gone > 0 {
        log(&format!(
            "removed {gone} old {side} backup{} (the newest {KEEP_BACKUPS} stay)",
            if gone == 1 { "" } else { "s" }
        ));
    }
}

/// A new folder for a save about to be replaced: `<save>-backups/<date> <side>`. With `prune`, the oldest of that side
/// beyond [`KEEP_BACKUPS`] go now.
pub(crate) fn backup_dir(save: &Path, side: &str, prune: bool) -> io::Result<PathBuf> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let base = format!("{} {side}", stamp(now, '.'));
    let dir = sibling(save, "-backups");
    let mut out = dir.join(&base);
    let mut k = 2;
    while files::exists(&out) {
        out = dir.join(format!("{base} {k}"));
        k += 1;
    }
    std::fs::create_dir_all(&out)?;
    if prune {
        prune_backups(save, side, &out);
    }
    Ok(out)
}

/// The cloud's file of the setting `v` (`NXKIT_SAVE_SYNC`, the values of `DM_SAVE_SYNC`) for the title `title`:
/// `<folder>/<title ID>/save.dmsync`, or `<folder>/save.dmsync` when the folder is the game's own. `icloud` is
/// `iCloud Drive/nxnative Saves`, the folder of the old engine, so both engines and the iPhone app share it. `None`:
/// sync is off.
pub(crate) fn cloud_path(v: Option<&str>, title: &str, home: Option<&Path>) -> Option<PathBuf> {
    let v = v.filter(|v| !v.is_empty() && *v != "0")?;
    let dir = if v == "icloud" {
        let drive = home?.join("Library/Mobile Documents/com~apple~CloudDocs");
        match std::fs::metadata(&drive) {
            Ok(_) => drive.join("nxnative Saves"),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                log(
                    "off: this app may not open iCloud Drive (System Settings > Privacy & Security > Files and Folders)",
                );
                return None;
            }
            Err(_) => {
                log(&format!(
                    "off: iCloud Drive is not on for this Mac (no {})",
                    drive.display()
                ));
                return None;
            }
        }
    } else {
        let t = v.trim_end_matches('/');
        PathBuf::from(if t.is_empty() { "/" } else { t })
    };
    let id = crate::pack::upper_title(title);
    if id.is_empty() || id.contains('/') {
        return None;
    }
    // The game's own folder chosen instead of the one above it: taken as it is, not given a second <title ID>.
    let own = dir
        .file_name()
        .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(&id));
    Some(if own {
        dir.join("save.dmsync")
    } else {
        dir.join(id).join("save.dmsync")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_backup_name_is_recognised_by_its_form() {
        assert!(is_backup("2026-11-21 10.00.00 local", "local"));
        assert!(is_backup("2026-11-21 10.00.00 cloud 3", "cloud"));
        for bad in [
            "2026-11-21 10.00.00 cloud",
            "2026-11-21 10:00:00 local",
            "x local",
            "2026-11-21 10.00.00 local ",
            "2026-11-21 10.00.00 local 2a",
        ] {
            assert!(!is_backup(bad, "local"), "{bad}");
        }
    }

    #[test]
    fn the_cloud_file_is_named_after_the_title() {
        let p = |v: &str| cloud_path(Some(v), "0100abc", None);
        assert_eq!(p("/c/"), Some(PathBuf::from("/c/0100ABC/save.dmsync")));
        assert_eq!(
            p("/c/0100Abc"),
            Some(PathBuf::from("/c/0100Abc/save.dmsync"))
        );
        assert_eq!(
            (p("0"), p(""), cloud_path(None, "1", None)),
            (None, None, None)
        );
        assert_eq!(cloud_path(Some("/c"), "a/b", None), None);
        assert_eq!(p("icloud"), None, "no home folder");
    }

    #[test]
    fn the_base_is_read_from_its_line() {
        let d = std::env::temp_dir().join(format!("nxkit-save-sync-base-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let save = d.join("save");
        assert_eq!(read_base(&save), "");
        write_base(&save, "0123456789abcdef0123456789abcdef");
        assert_eq!(read_base(&save), "0123456789abcdef0123456789abcdef");
        assert!(
            std::fs::read_to_string(sibling(&save, ".sync"))
                .unwrap()
                .starts_with("# save sync")
        );
        std::fs::remove_dir_all(d).unwrap();
    }
}
