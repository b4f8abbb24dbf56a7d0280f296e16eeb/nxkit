//! The cache of compiled models.

use std::fs;
use std::path::{Path, PathBuf};

use objc2_foundation::NSProcessInfo;

use crate::error::{CoreMlError, Result};

/// The build of macOS, for example `Version 15.1 (Build 24B83)`. Core ML compiles for the OS it runs on, so a cache key
/// must include it: a compiled model of another build may not load or may run slower.
pub fn os_build_tag() -> String {
    NSProcessInfo::processInfo()
        .operatingSystemVersionString()
        .to_string()
}

fn io(what: &str, path: &Path, e: std::io::Error) -> CoreMlError {
    CoreMlError::Io(format!("{what} {}: {e}", path.display()))
}

/// The directory in the cache for a key. Anything but letters, digits, `.`, `-` and `_` becomes `_`, and a leading
/// `.` too, so a key cannot leave the cache directory.
pub(crate) fn entry_path(cache_dir: &Path, key: &str) -> Result<PathBuf> {
    if key.is_empty() {
        return Err(CoreMlError::Invalid("the cache key is empty".into()));
    }
    let mut name: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.starts_with('.') {
        name.replace_range(0..1, "_");
    }
    Ok(cache_dir.join(format!("{name}.mlmodelc")))
}

/// Copies a directory tree.
pub(crate) fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to).map_err(|e| io("create", to, e))?;
    for entry in fs::read_dir(from).map_err(|e| io("read", from, e))? {
        let entry = entry.map_err(|e| io("read", from, e))?;
        let dest = to.join(entry.file_name());
        let kind = entry
            .file_type()
            .map_err(|e| io("stat", &entry.path(), e))?;
        if kind.is_dir() {
            copy_tree(&entry.path(), &dest)?;
        } else {
            fs::copy(entry.path(), &dest).map_err(|e| io("copy", &entry.path(), e))?;
        }
    }
    Ok(())
}

/// Moves a freshly compiled model into its cache entry. It is copied to a temporary name next to the entry and then
/// renamed, so a reader never sees a half-written entry and two processes that race both end with a whole one.
pub(crate) fn store(compiled: &Path, entry: &Path) -> Result<()> {
    let dir = entry.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir).map_err(|e| io("create", dir, e))?;
    let tmp = dir.join(format!(".tmp-{}-{}", std::process::id(), nanos()));
    copy_tree(compiled, &tmp)?;
    if let Err(e) = fs::rename(&tmp, entry) {
        let _ = fs::remove_dir_all(&tmp);
        // Another process stored the same key first: its entry is as good as ours.
        if !entry.is_dir() {
            return Err(io("rename to", entry, e));
        }
    }
    Ok(())
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_names_stay_inside_the_cache() {
        let d = Path::new("/c");
        assert_eq!(
            entry_path(d, "net-v1/../x y").ok(),
            Some(PathBuf::from("/c/net-v1_.._x_y.mlmodelc"))
        );
        assert_eq!(
            entry_path(d, "..").ok(),
            Some(PathBuf::from("/c/_..mlmodelc"))
        );
        assert!(entry_path(d, "").is_err());
    }

    #[test]
    fn store_copies_a_tree_and_keeps_the_first_entry() {
        let root = std::env::temp_dir().join(format!("nxkit-coreml-cache-{}", nanos()));
        let src = root.join("src");
        fs::create_dir_all(src.join("sub")).unwrap_or_default();
        fs::write(src.join("sub/a.bin"), b"one").unwrap_or_default();
        let entry = root.join("cache/k.mlmodelc");
        assert!(store(&src, &entry).is_ok());
        assert_eq!(
            fs::read(entry.join("sub/a.bin")).ok(),
            Some(b"one".to_vec())
        );
        fs::write(src.join("sub/a.bin"), b"two").unwrap_or_default();
        assert!(store(&src, &entry).is_ok());
        assert_eq!(
            fs::read(entry.join("sub/a.bin")).ok(),
            Some(b"one".to_vec())
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn os_build_tag_is_not_empty() {
        assert!(!os_build_tag().is_empty());
    }
}
