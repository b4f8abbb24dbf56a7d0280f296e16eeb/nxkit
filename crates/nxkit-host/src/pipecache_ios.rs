//! Pipeline caches use the writable sandbox, never a write probe inside the signed bundle.
use crate::{HostError, Result};
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};
struct Cache {
    folder: PathBuf,
    build: u64,
}
static CACHE: OnceLock<Cache> = OnceLock::new();
fn hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    })
}
/// Creates and records the writable cache directory and manifest identity.
pub fn configure(folder: &Path, manifest: &[u8]) -> Result<()> {
    std::fs::create_dir_all(folder).map_err(|e| HostError::io(folder, &e))?;
    CACHE
        .set(Cache {
            folder: folder.to_owned(),
            build: hash(manifest),
        })
        .map_err(|_| HostError::App("Pipeline cache was already configured".into()))
}
/// Returns the configured sandbox cache, without probing signed assets.
pub fn folder(_root: &Path) -> Option<PathBuf> {
    CACHE.get().map(|c| c.folder.clone())
}
/// Returns the configured manifest hash, or zero before configuration.
pub fn build_id(_root: &Path) -> u64 {
    CACHE.get().map_or(0, |c| c.build)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manifest_changes_identity() {
        assert_ne!(hash(b"{}"), hash(b"{ }"));
        assert_eq!(hash(b"x"), hash(b"x"));
    }
}
