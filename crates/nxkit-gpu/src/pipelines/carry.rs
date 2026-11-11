//! The list of recorded pipelines, kept across app builds.
//!
//! The descriptions in `pipelines.seen` (`record.rs`) hold the stems and entries of the shader libraries, the colour
//! state, the vertex layout and the key of the pipeline cache. None of that depends on the shader binaries: a rebuilt
//! app has the same descriptions for the same game, as long as the key scheme (`KEY_SCHEME`, `keys.rs`) is the same.
//! The archives do depend on the binaries, so they stay in the folder of the identity (`runtime.rs`) and go with it,
//! but the list is kept in `<root>/pipecache/seen-<scheme>.list`, outside the identity folders. A rebuild then starts
//! with the list of the earlier builds, and the start makes the pipelines of it before the first frame.
//!
//! Lists written by earlier host builds are in the identity folders as `pipelines.seen`. They are read as well and
//! merged into the new list (they have no scheme in them: they are taken to have the current one), and they are not
//! written any more. The text of the new list has a `#scheme <n>` line after the header; the reader of `record.rs`
//! skips it, as it skips every line that is not an entry.
//!
//! One more file belongs to the identity folder: `prewarm.done`, which the start writes when it made every pipeline of
//! the list. Without it the list is from an older build or the start did not finish, and the pipelines of the list
//! are neither in the shipped archive nor in an `rt-` archive of this identity, as far as the host knows. The start
//! then waits for all of them (see `prewarm.rs`).

use std::path::{Path, PathBuf};

use super::record::Seen;

/// The legacy list file in an identity folder.
pub(super) const LEGACY_FILE: &str = "pipelines.seen";
/// The marker of an identity folder whose start made every pipeline of the list.
pub(super) const DONE_FILE: &str = "prewarm.done";

/// The file of the list for a key scheme, under `<root>/pipecache`.
pub(super) fn list_path(base: &Path, scheme: u32) -> PathBuf {
    base.join(format!("seen-{scheme}.list"))
}

/// What [`load`] found.
pub(super) struct Loaded {
    /// The list: the carried file first, then what the identity folders had that it lacked.
    pub(super) seen: Seen,
    /// Entries that came from the identity folders and not from the carried file. When there are some, the list has
    /// to be written to its file again.
    pub(super) migrated: usize,
}

fn read(path: &Path) -> Option<Seen> {
    let text = std::fs::read_to_string(path).ok()?;
    match Seen::parse(&text) {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("nxkit-gpu: {} is not used: {e}", path.display());
            None
        }
    }
}

/// Reads the list of the key scheme and merges the lists of the identity folders under `base` into it.
pub(super) fn load(base: &Path, scheme: u32) -> Loaded {
    let mut seen = read(&list_path(base, scheme)).unwrap_or_default();
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(base)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("id-"))
        .map(|e| e.path())
        .collect();
    dirs.sort();
    let mut migrated = 0;
    for d in dirs {
        if let Some(old) = read(&d.join(LEGACY_FILE)) {
            migrated += seen.merge(&old);
        }
    }
    Loaded { seen, migrated }
}

/// The text of the carried list: the text of `seen` with the scheme after the header.
pub(super) fn text(seen: &Seen, scheme: u32) -> String {
    let t = seen.to_text();
    match t.split_once('\n') {
        Some((header, rest)) => format!("{header}\n#scheme {scheme}\n{rest}"),
        None => t,
    }
}

/// Whether the start has to wait for every pipeline of the list: the list is not empty and the identity folder `dir`
/// has no `prewarm.done`.
pub(super) fn needs_full_start(dir: &Path, seen: &Seen) -> bool {
    !seen.is_empty() && !dir.join(DONE_FILE).is_file()
}

#[cfg(test)]
mod tests;
