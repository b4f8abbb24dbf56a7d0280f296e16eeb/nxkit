//! The app of the old engine (nxnative) at `-o`, and its save (issue 43).
//!
//! NX2Apple built such an app at the same place before the game moved to nxkit. nxnative keeps the save outside the
//! app, in `~/Library/Application Support/nxnative/<title id>/save`, or in `<library>/save/<title id>` when the folder
//! that holds the app is a game library (it has the file `.nx2apple-library`). Its launcher writes the folder it last
//! started the game on to `<support>/save-location`, and that folder is the save the player last played
//! (`current_save` of `dm/mac/launcher.c`).
//!
//! The save folder of nxnative is the committed save of the transaction in `dm/host/hle_fs.c`: the work folder
//! `<save>.work` and the folder `<save>.new` are siblings of it, a device save is the sibling `<save>_device`, and a
//! guest name that collides on APFS is the attribute `org.d2rmobile.guest-name`. nxkit-svc-fs keeps its save in the same
//! layout (`commit.rs`, `names.rs`), so the committed folder is copied as it is into `<run>/save/<title id>`. The work
//! folder is not copied: it holds what the game wrote after its last commit, which nxnative drops at the next start too.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Whether `app` is an app of nxnative: its `dm` folder and its launcher `nxnative` or its host `dmhost`. Every app of
/// nxnative has the host (`dm/mac/pack_app.py` signs it). An app packed before the launcher existed has only the host
/// (issue 48).
pub(super) fn is_app(app: &Path) -> bool {
    let macos = app.join("Contents/MacOS");
    (macos.join("nxnative").is_file() || macos.join("dmhost").is_file())
        && app.join("Contents/Resources/dm").is_dir()
}

/// Files that macOS writes into any folder and that are not part of a save (`not_save` of the launcher).
fn not_save(name: &OsStr) -> bool {
    let n = name.as_encoded_bytes();
    n.starts_with(b"._") || n == b".DS_Store"
}

fn has_entries(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut d| d.any(|e| e.is_ok_and(|e| !not_save(&e.file_name()))))
}

pub(super) fn device(save: &Path) -> PathBuf {
    let mut s = save.as_os_str().to_os_string();
    s.push("_device");
    PathBuf::from(s)
}

/// Whether the save folder `save` holds a save: the folder itself or its device save.
fn has_save(save: &Path) -> bool {
    has_entries(save) || has_entries(&device(save))
}

/// The save folder that nxnative would start the game `id` on, for the app `app` and the home folder `home`, or `None`
/// when it holds no save.
pub(super) fn current_save(home: &Path, app: &Path, id: u64) -> Option<PathBuf> {
    let id = format!("{id:016X}");
    let sup = home.join("Library/Application Support/nxnative").join(&id);
    let recorded = std::fs::read_to_string(sup.join("save-location"))
        .ok()
        .map(|s| PathBuf::from(s.lines().next().unwrap_or("")))
        .filter(|p| !p.as_os_str().is_empty() && p.is_dir());
    let library = app
        .parent()
        .filter(|d| d.join(".nx2apple-library").exists())
        .map(|d| d.join("save").join(&id));
    let save = recorded
        .or_else(|| library.filter(|l| has_save(l)))
        .unwrap_or_else(|| sup.join("save"));
    has_save(&save).then_some(save)
}

/// Copies the save of the nxnative app `app` into the run folder `run` of the new app: `<run>/save/<title id>` and its
/// device save. The old save stays where it is, because an older build may still read it. The host of the new app
/// moves the copy to `~/Library/Application Support/nxkit/<title id>/save` at its first start, when no save is there
/// (R67). Returns what to tell the player, or `None` when there is no save. The caller checks that the new app has no
/// save yet.
pub(super) fn carry(
    home: &Path,
    app: &Path,
    id: u64,
    run: &Path,
) -> Result<Option<String>, String> {
    let Some(from) = current_save(home, app, id) else {
        return Ok(None);
    };
    let to = run.join("save").join(format!("{id:016X}"));
    std::fs::create_dir_all(run.join("save")).map_err(|e| super::io(&run.join("save"), e))?;
    for (f, t) in [(from.clone(), to.clone()), (device(&from), device(&to))] {
        if f.is_dir() {
            nxb_sys::clone::clone_or_copy(&f, &t).map_err(|e| {
                format!(
                    "{}: the save of the old app (nxnative) could not be copied from {}: {e}",
                    t.display(),
                    f.display()
                )
            })?;
        }
    }
    Ok(Some(format!(
        "save: copied from the old app (nxnative) {} to {}; the old save stays",
        from.display(),
        to.display()
    )))
}
