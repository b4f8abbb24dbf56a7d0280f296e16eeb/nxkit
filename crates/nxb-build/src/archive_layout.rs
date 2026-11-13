//! How the pipeline archive reaches the app: one merged `.mtl4a` file, or the batch archives as shards with an index.
//!
//! The host (`MTL4Archive`) reads an archive file whole into memory, 4.8 GB for a big title. The shards are the batch
//! archives that the build compiles anyway, each a file of its own, and `index.nxpi` tells the host which pipeline is
//! in which one (`nxb_archive::shards`, `nxkit_abi::shards`). The merge (`metal-pack`) and its copy of the whole
//! archive in the store are not needed for them. The batches are content keyed in the store, so a rebuild reuses the
//! batch archives that did not change (docs/early-cutoff.md).
//!
//! `NXB_ARCHIVE_LAYOUT=shards|merged|both` picks it for the standalone build. The default is `shards`. The Mac app of the
//! Darwin-Metal host reads the merged file only and always gets `merged`.

use std::path::{Path, PathBuf};

use crate::graph::Build;

/// The files of the archive that the build writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// One merged archive file.
    Merged,
    /// The batch archives and the index.
    Shards,
    /// Both (the app is as large as the two together).
    Both,
}

impl Layout {
    /// The layout named `s`: `merged`, `shards` or `both`.
    pub fn parse(s: &str) -> Option<Layout> {
        match s.trim() {
            "merged" => Some(Layout::Merged),
            "shards" => Some(Layout::Shards),
            "both" => Some(Layout::Both),
            _ => None,
        }
    }

    /// `NXB_ARCHIVE_LAYOUT`, else the shards. A value that names no layout is the default too.
    pub fn from_env() -> Layout {
        std::env::var("NXB_ARCHIVE_LAYOUT")
            .ok()
            .and_then(|v| Layout::parse(&v))
            .unwrap_or(Layout::Shards)
    }

    /// Whether the merged file is made.
    pub fn merges(self) -> bool {
        self != Layout::Shards
    }

    /// Whether the shards are written.
    pub fn shards(self) -> bool {
        self != Layout::Merged
    }
}

/// Where the build writes the shards: `<work>/pso/mac/pipelines`.
pub(crate) fn shard_dir(b: &Build) -> PathBuf {
    b.work.join("pso/mac").join(nxkit_abi::shards::SHARD_DIR)
}

/// Writes the shard folder of a streamed archive and says what it holds.
pub(crate) fn publish(b: &Build, rep: &nxb_archive::Report) -> Result<String, String> {
    publish_at(&shard_dir(b), rep)
}

/// Publish archives into the selected platform folder.
pub(crate) fn publish_at(dir: &Path, rep: &nxb_archive::Report) -> Result<String, String> {
    if rep.shards.is_empty() {
        return Err("not enough input files: no batch archive was compiled".into());
    }
    let w = nxb_archive::shards::write(dir, &rep.shards)?;
    Ok(format!(
        "{} shards, {} MB ({} kept from the last build, {} dropped)",
        w.files,
        w.bytes >> 20,
        w.kept,
        w.removed
    ))
}

/// Copies the shard folder `from` to `<run>/pipelines`, replacing what is there. The index goes last. Returns the
/// number of shard files.
pub(crate) fn copy_to_run(from: &Path, run: &Path) -> Result<usize, String> {
    let io = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    let to = run.join(nxkit_abi::shards::SHARD_DIR);
    let _ = std::fs::remove_dir_all(&to);
    std::fs::create_dir_all(&to).map_err(|e| io(&to, e))?;
    let index = nxkit_abi::shards::INDEX_FILE;
    let ext = format!(".{}", nxkit_abi::shards::SHARD_EXT);
    let mut n = 0;
    for e in std::fs::read_dir(from).map_err(|e| io(from, e))?.flatten() {
        if e.file_name().to_string_lossy().ends_with(&ext) {
            std::fs::copy(e.path(), to.join(e.file_name())).map_err(|x| io(&e.path(), x))?;
            n += 1;
        }
    }
    std::fs::copy(from.join(index), to.join(index)).map_err(|e| io(&from.join(index), e))?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layout_is_named() {
        assert_eq!(Layout::parse("merged"), Some(Layout::Merged));
        assert_eq!(Layout::parse(" shards\n"), Some(Layout::Shards));
        assert_eq!(Layout::parse("both"), Some(Layout::Both));
        assert_eq!(Layout::parse("single"), None);
        assert_eq!(Layout::parse(""), None);
    }

    #[test]
    fn each_layout_says_what_it_writes() {
        let w = |l: Layout| (l.merges(), l.shards());
        assert_eq!(w(Layout::Merged), (true, false));
        assert_eq!(w(Layout::Shards), (false, true));
        assert_eq!(w(Layout::Both), (true, true));
    }

    #[test]
    fn the_shard_folder_is_copied_whole_into_the_run_folder() {
        let d = std::env::temp_dir().join(format!("nxb-layout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (from, run) = (d.join("from"), d.join("run"));
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(run.join("pipelines")).unwrap();
        std::fs::write(run.join("pipelines/old.mtl4archive"), b"old").unwrap();
        std::fs::write(from.join("a.mtl4archive"), b"A").unwrap();
        std::fs::write(from.join("b.mtl4archive"), b"B").unwrap();
        std::fs::write(from.join("index.nxpi"), b"I").unwrap();
        std::fs::write(from.join("note.txt"), b"not a shard").unwrap();
        assert_eq!(copy_to_run(&from, &run), Ok(2));
        let to = run.join("pipelines");
        assert_eq!(std::fs::read(to.join("b.mtl4archive")).unwrap(), b"B");
        assert!(to.join("index.nxpi").is_file());
        assert!(!to.join("old.mtl4archive").exists());
        assert!(!to.join("note.txt").exists());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_folder_without_an_index_is_an_error() {
        let d = std::env::temp_dir().join(format!("nxb-layout-noix-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("from")).unwrap();
        assert!(copy_to_run(&d.join("from"), &d.join("run")).is_err());
        std::fs::remove_dir_all(&d).unwrap();
    }
}
