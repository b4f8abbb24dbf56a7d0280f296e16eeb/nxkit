//! Where the runtime pipeline cache of a title lives and what makes it out of date (`nxkit-gpu`, `pipelines/runtime.rs`).
//!
//! The cache is a folder of the title that the host can write: the run folder when it is writable, else
//! `~/Library/Caches/nxkit/<title id>/` (`NXKIT_PIPELINE_CACHE_DIR` names another one). The GPU crate adds the host
//! build, the macOS build and the GPU to the identity of its sub-folder. This module adds the app build: a hash of
//! `game.json`, the shard index, the single pipeline archive (size and time) and the shader folder (time), which all
//! change when the build step runs again.

use std::path::Path;
#[cfg(any(not(target_os = "ios"), test))]
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

#[cfg(not(target_os = "ios"))]
use crate::json;

#[cfg(target_os = "ios")]
pub use sandbox::{configure, folder};

#[cfg(any(target_os = "ios", test))]
mod sandbox {
    use crate::{HostError, Result};
    use std::{
        path::{Path, PathBuf},
        sync::OnceLock,
    };

    static FOLDER: OnceLock<PathBuf> = OnceLock::new();

    /// Records the writable sandbox cache without probing signed assets.
    pub fn configure(folder: &Path) -> Result<()> {
        std::fs::create_dir_all(folder).map_err(|e| HostError::io(folder, &e))?;
        FOLDER
            .set(folder.to_owned())
            .map_err(|_| HostError::App("Pipeline cache was already configured".into()))
    }

    /// Returns the configured sandbox cache. The bundle remains read-only.
    pub fn folder(_root: &Path) -> Option<PathBuf> {
        FOLDER.get().cloned()
    }

    #[cfg(test)]
    #[test]
    fn uses_only_the_sandbox_for_writes() {
        let root = super::tests::tmp("sandbox");
        let bundle = root.join("game.app");
        let cache = root.join("cache");
        std::fs::create_dir(&bundle).unwrap();
        configure(&cache).unwrap();
        assert_eq!(folder(&bundle), Some(cache));
        assert_eq!(std::fs::read_dir(&bundle).unwrap().count(), 0);
        assert!(configure(&root.join("other")).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(not(target_os = "ios"))]
const PROBE: &str = ".nxkit-write-probe";

/// The folder for the runtime cache, or none when there is no place to write.
#[cfg(not(target_os = "ios"))]
pub fn folder(root: &Path) -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("NXKIT_PIPELINE_CACHE_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    if writable(&root.join("pipecache")) {
        return Some(root.to_path_buf());
    }
    let title = title_id(&std::fs::read_to_string(root.join("game.json")).ok()?)?;
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    let dir = PathBuf::from(home).join("Library/Caches/nxkit").join(title);
    writable(&dir.join("pipecache")).then_some(dir)
}

/// Whether a file can be made in `dir`, which is made if it is missing.
#[cfg(not(target_os = "ios"))]
fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!("{PROBE}-{}", std::process::id()));
    std::fs::create_dir_all(dir).is_ok()
        && std::fs::write(&probe, b"").is_ok()
        && std::fs::remove_file(&probe).is_ok()
}

/// The title id of a `game.json` text, made of hex digits (it names a folder).
#[cfg(not(target_os = "ios"))]
fn title_id(text: &str) -> Option<String> {
    let v = json::parse(text).ok()?;
    let id = v.get("title_id")?.as_str()?;
    (!id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric())).then(|| id.to_string())
}

fn fnv(h: u64, bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .chain(&[0xff])
        .fold(h, |h, c| (h ^ u64::from(*c)).wrapping_mul(0x100_0000_01b3))
}

/// Size and modification time of a path as bytes, empty when it is missing.
fn stamp(p: &Path) -> Vec<u8> {
    let Ok(m) = std::fs::metadata(p) else {
        return Vec::new();
    };
    let t = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    format!("{}:{t}", m.len()).into_bytes()
}

/// A number that changes when the app is built again.
pub fn build_id(root: &Path) -> u64 {
    let read = |p: &Path| std::fs::read(p).unwrap_or_default();
    let mut h = 0xcbf2_9ce4_8422_2325;
    h = fnv(h, &read(&root.join("game.json")));
    h = fnv(
        h,
        &read(
            &root
                .join(nxkit_abi::shards::SHARD_DIR)
                .join(nxkit_abi::shards::INDEX_FILE),
        ),
    );
    h = fnv(h, &stamp(&root.join("pipelines.mtl4archive")));
    fnv(h, &stamp(&root.join("shaders")))
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nxkit-pipecache-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    #[cfg(not(target_os = "ios"))]
    fn the_title_id_names_a_folder_or_nothing() {
        assert_eq!(
            title_id(r#"{"title_id": "01007EF00011E000"}"#).as_deref(),
            Some("01007EF00011E000")
        );
        assert_eq!(title_id(r#"{"title_id": "../x"}"#), None);
        assert_eq!(title_id(r#"{"title_id": ""}"#), None);
        assert_eq!(title_id("{}"), None);
        assert_eq!(title_id("not json"), None);
    }

    #[test]
    #[cfg(not(target_os = "ios"))]
    fn a_writable_run_folder_is_the_cache_root() {
        let d = tmp("root");
        if std::env::var_os("NXKIT_PIPELINE_CACHE_DIR").is_none() {
            assert_eq!(folder(&d).as_deref(), Some(d.as_path()));
        }
        // the probe file does not stay
        assert_eq!(std::fs::read_dir(d.join("pipecache")).unwrap().count(), 0);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn the_build_id_follows_the_files_of_the_build() {
        let d = tmp("build");
        std::fs::write(d.join("game.json"), b"{}").unwrap();
        let a = build_id(&d);
        assert_eq!(a, build_id(&d));
        std::fs::write(d.join("game.json"), b"{ }").unwrap();
        let b = build_id(&d);
        assert_ne!(a, b);
        std::fs::create_dir_all(d.join("pipelines")).unwrap();
        std::fs::write(d.join("pipelines/index.nxpi"), b"x").unwrap();
        assert_ne!(b, build_id(&d));
        let c = build_id(&d);
        std::fs::write(d.join("pipelines.mtl4archive"), b"archive").unwrap();
        let e = build_id(&d);
        assert_ne!(c, e);
        std::fs::create_dir(d.join("shaders")).unwrap();
        assert_ne!(e, build_id(&d));
        // Artifact changes must invalidate even with the same manifest bytes.
        assert_eq!(std::fs::read(d.join("game.json")).unwrap(), b"{ }");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
