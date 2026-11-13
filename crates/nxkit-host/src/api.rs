//! Native engine contract. UIKit and touch tracking live in the Swift shell.
use std::path::{Path, PathBuf};

/// Read-only application assets and separate writable sandbox roots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// The signed bundle. Runtime code must never create files here.
    pub bundle: PathBuf,
    /// Persistent saves and settings.
    pub data: PathBuf,
    /// Regenerable pipeline caches and temporary files.
    pub cache: PathBuf,
}

impl Paths {
    /// Derive app-owned roots from UIKit's bundle and process sandbox home.
    /// Requires disjoint absolute paths; no game data or credentials are copied.
    pub fn new(bundle: &Path, home: &Path) -> Result<Self, String> {
        if !bundle.is_absolute() || !home.is_absolute() {
            return Err("The app bundle and sandbox home must be absolute paths".into());
        }
        if bundle.starts_with(home) || home.starts_with(bundle) {
            return Err("The signed bundle must be separate from writable app data".into());
        }
        if bundle
            .components()
            .chain(home.components())
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err("Parent traversal is not allowed in app roots".into());
        }
        Ok(Self {
            bundle: bundle.to_owned(),
            data: home.join("Library/Application Support/nxkit"),
            cache: home.join("Library/Caches/nxkit"),
        })
    }

    /// Check the bundle input and create only the app's writable directories.
    pub fn prepare(&self) -> Result<(), String> {
        if !self.bundle.join("game.json").is_file() {
            return Err("This app has no bundled game.json".into());
        }
        let bundle = self.bundle.canonicalize().map_err(|e| e.to_string())?;
        // Check both destinations before creating either. An existing sandbox
        // ancestor can be a symlink even when the supplied roots are disjoint.
        for path in [&self.data, &self.cache] {
            let mut ancestor = path.as_path();
            while !ancestor.exists() {
                ancestor = ancestor.parent().ok_or("App data has no existing parent")?;
            }
            if ancestor
                .canonicalize()
                .map_err(|e| e.to_string())?
                .starts_with(&bundle)
            {
                return Err("App data must not resolve inside the signed bundle".into());
            }
        }
        for path in [&self.data, &self.cache] {
            std::fs::create_dir_all(path)
                .map_err(|e| format!("Cannot create app data folder: {e}"))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn writes_are_outside_signed_bundle() {
        let p = Paths::new(Path::new("/apps/game.app"), Path::new("/data/app")).unwrap();
        assert_eq!(
            p.data,
            Path::new("/data/app/Library/Application Support/nxkit")
        );
        assert_eq!(p.cache, Path::new("/data/app/Library/Caches/nxkit"));
        assert!(!p.data.starts_with(p.bundle));
    }
    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_data_before_writing_bundle() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("nxkit-paths-{}-{unique}", std::process::id()));
        let bundle = root.join("game.app");
        let home = root.join("home");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(bundle.join("game.json"), "{}").unwrap();
        std::os::unix::fs::symlink(&bundle, home.join("Library")).unwrap();
        assert!(Paths::new(&bundle, &home).unwrap().prepare().is_err());
        assert!(!bundle.join("Application Support").exists());
        assert!(!bundle.join("Caches").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn rejects_relative_overlapping_and_parent_paths() {
        for (a, b) in [
            ("app", "/data"),
            ("/app", "data"),
            ("/app", "/app/data"),
            ("/data/app", "/data"),
            ("/app/../other", "/data"),
        ] {
            assert!(Paths::new(Path::new(a), Path::new(b)).is_err());
        }
    }
}

/// ABI-stable normalized finger, owned by the caller for the duration of a call.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Touch {
    /// Stable finger identifier in the range 0 through 15.
    pub id: u32,
    /// Horizontal coordinate normalized to the drawable width.
    pub x: f32,
    /// Vertical coordinate normalized to the drawable height.
    pub y: f32,
}
