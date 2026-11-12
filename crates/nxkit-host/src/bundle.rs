//! Resolve immutable bundle assets before redirecting host saves and temporary files.
use crate::{Game, HostError, Result, json};
use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

fn invalid(message: impl Into<String>) -> HostError {
    HostError::Manifest(message.into())
}
fn canonical(path: &Path) -> Result<PathBuf> {
    path.canonicalize().map_err(|e| HostError::io(path, &e))
}
fn relative(path: &Path) -> Result<()> {
    let text = path
        .to_str()
        .ok_or_else(|| invalid("asset path is not UTF-8"))?;
    if text.is_empty()
        || text.contains(['\\', ':', '\0'])
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(invalid(format!(
            "asset path must be relative without traversal: {text:?}"
        )));
    }
    Ok(())
}
/// Resolve each component, including parents of an intentionally absent empty RomFS.
fn asset(root: &Path, path: &Path, directory: bool, missing: bool) -> Result<Option<PathBuf>> {
    relative(path)?;
    let mut current = root.to_path_buf();
    let mut absent = false;
    for component in path.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(_) => {
                let resolved = canonical(&current)?;
                if !resolved.starts_with(root) {
                    return Err(invalid(format!("asset escapes bundle: {}", path.display())));
                }
                current = resolved;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && missing => {
                absent = true;
            }
            Err(e) => return Err(HostError::io(&current, &e)),
        }
    }
    if absent {
        return Ok(None);
    }
    if directory != current.is_dir() || (!directory && !current.is_file()) {
        return Err(invalid(format!(
            "asset has wrong file type: {}",
            path.display()
        )));
    }
    if directory {
        check_directory(root, &current)?;
    }
    Ok(Some(current))
}
/// Directory assets must not conceal escapes in their descendants either.
fn check_directory(root: &Path, path: &Path) -> Result<()> {
    let mut pending = vec![path.to_path_buf()];
    let mut visited = HashSet::new();
    while let Some(dir) = pending.pop() {
        if !visited.insert(dir.clone()) {
            continue;
        }
        for entry in fs::read_dir(&dir).map_err(|e| HostError::io(&dir, &e))? {
            let entry = entry.map_err(|e| HostError::io(&dir, &e))?;
            let resolved = canonical(&entry.path())?;
            if !resolved.starts_with(root) {
                return Err(invalid(format!(
                    "directory asset escapes bundle: {}",
                    entry.path().display()
                )));
            }
            if resolved.is_dir() {
                pending.push(resolved);
            } else if !resolved.is_file() {
                return Err(invalid("bundle contains a non-file asset"));
            }
        }
    }
    Ok(())
}
fn file(root: &Path, path: &mut PathBuf) -> Result<()> {
    *path = asset(root, path, false, false)?.ok_or_else(|| invalid("missing file asset"))?;
    Ok(())
}
fn empty(path: &Path) -> Result<bool> {
    Ok(fs::read_dir(path)
        .map_err(|e| HostError::io(path, &e))?
        .next()
        .transpose()
        .map_err(|e| HostError::io(path, &e))?
        .is_none())
}

/// Read a packaged game. Asset paths become absolute; `root` becomes writable `data`.
/// `data` is the app's private container, not a directory inside the signed bundle.
pub fn read(bundle: &Path, data: &Path) -> Result<Game> {
    let bundle = canonical(bundle)?;
    if !bundle.is_dir() {
        return Err(invalid("bundle is not a directory"));
    }
    let manifest = asset(&bundle, Path::new("game.json"), false, false)?
        .ok_or_else(|| invalid("missing game.json"))?;
    let source = fs::read_to_string(&manifest).map_err(|e| HostError::io(&manifest, &e))?;
    let raw = json::parse(&source)?;
    let romfs_empty = match raw.get("romfs_empty") {
        None => false,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| invalid("romfs_empty must be a boolean"))?,
    };
    // Parse the same bytes inspected above rather than reopening a mutable path.
    let mut game = Game::parse(&bundle, &source)?;
    if game.title_id.len() != 16 || !game.title_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid(
            "title_id must contain exactly 16 hexadecimal digits",
        ));
    }
    // The upstream parser treats non-string optional paths as absent. Reject these
    // explicitly so malformed paths cannot silently bypass validation.
    for key in ["npdm", "patches"] {
        if raw
            .get(key)
            .is_some_and(|v| !matches!(v, json::Value::Null) && v.as_str().is_none())
        {
            return Err(invalid(format!("{key} must be a string or null")));
        }
    }
    let romfs = asset(&bundle, &game.romfs, true, romfs_empty)?;
    if romfs_empty {
        if let Some(path) = &romfs {
            if !empty(path)? {
                return Err(invalid("romfs_empty is true but bundle RomFS is not empty"));
            }
        }
    }
    file(&bundle, &mut game.dylib)?;
    for path in [&mut game.npdm, &mut game.patches].into_iter().flatten() {
        file(&bundle, path)?;
    }
    for module in &mut game.modules {
        for path in [
            &mut module.image,
            &mut module.imports,
            &mut module.exports,
            &mut module.gates,
            &mut module.fixups,
        ] {
            file(&bundle, path)?;
        }
    }
    for aoc in &mut game.aoc {
        aoc.path = asset(&bundle, &aoc.path, true, false)?.ok_or_else(|| invalid("missing DLC"))?;
    }
    if let Some(font) = &mut game.settings.shared_font {
        let mut path = PathBuf::from(&*font);
        file(&bundle, &mut path)?;
        *font = path
            .to_str()
            .ok_or_else(|| invalid("font path is not UTF-8"))?
            .to_owned();
    }
    // Verify existing ancestors before making any directory, including a symlinked parent.
    let mut ancestor = data;
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .ok_or_else(|| invalid("data has no existing parent"))?;
    }
    if canonical(ancestor)?.starts_with(&bundle) {
        return Err(invalid("writable data must be outside the signed bundle"));
    }
    fs::create_dir_all(data).map_err(|e| HostError::io(data, &e))?;
    let data = canonical(data)?;
    if data.starts_with(&bundle) {
        return Err(invalid("writable data must be outside the signed bundle"));
    }
    game.romfs = match romfs {
        Some(path) => path,
        None => {
            let path = data.join("empty-romfs");
            match fs::symlink_metadata(&path) {
                Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
                    return Err(invalid("empty-romfs must be an ordinary private directory"));
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    fs::create_dir(&path).map_err(|e| HostError::io(&path, &e))?
                }
                Err(e) => return Err(HostError::io(&path, &e)),
            }
            if !empty(&path)? {
                return Err(invalid("private empty-romfs contains files"));
            }
            canonical(&path)?
        }
    };
    game.root = data;
    Ok(game)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture {
        root: PathBuf,
        bundle: PathBuf,
        data: PathBuf,
        source: String,
    }
    impl Fixture {
        fn new(extra: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "nx2ipa-bundle-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let bundle = root.join("App.app");
            let data = root.join("data");
            fs::create_dir_all(&bundle).unwrap();
            let source = crate::manifest::sample(extra, crate::module::samples::NEAR)
                .replace("\"romfs\": \"romfs\"", "\"romfs\": \"nxkit/romfs\"")
                .replace(
                    "\"dylib\": \"nxgame.dylib\"",
                    "\"dylib\": \"Frameworks/nxgame.dylib\"",
                );
            let game = Game::parse(&bundle, &source).unwrap();
            let mut paths = vec![game.dylib];
            paths.extend(game.npdm);
            paths.extend(game.patches);
            for m in game.modules {
                paths.extend([m.image, m.imports, m.exports, m.gates, m.fixups]);
            }
            if let Some(font) = game.settings.shared_font {
                paths.push(font.into());
            }
            for path in paths {
                let path = bundle.join(path);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, []).unwrap();
            }
            for aoc in game.aoc {
                fs::create_dir_all(bundle.join(aoc.path)).unwrap();
            }
            let fixture = Self {
                root,
                bundle,
                data,
                source,
            };
            fixture.write(&fixture.source);
            fixture
        }
        fn write(&self, source: &str) {
            fs::write(self.bundle.join("game.json"), source).unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn all_paths_stay_in_bundle_while_root_is_writable() {
        let f = Fixture::new(
            "\"npdm\":\"nxkit/main.npdm\",\"patches\":\"nxkit/patches.json\",\"aoc\":[{\"index\":1,\"title_id\":\"01007EF00011F001\",\"version\":0,\"path\":\"nxkit/aoc/1\"}],",
        );
        fs::create_dir_all(f.bundle.join("nxkit/romfs")).unwrap();
        let g = read(&f.bundle, &f.data).unwrap();
        assert_eq!(g.root, canonical(&f.data).unwrap());
        for p in [
            g.romfs,
            g.dylib,
            g.npdm.unwrap(),
            g.patches.unwrap(),
            g.aoc[0].path.clone(),
        ] {
            assert!(p.starts_with(canonical(&f.bundle).unwrap()));
        }
        for m in g.modules {
            for p in [m.image, m.imports, m.exports, m.gates, m.fixups] {
                assert!(p.starts_with(canonical(&f.bundle).unwrap()));
            }
        }
    }
    #[test]
    fn absent_romfs_requires_explicit_true() {
        for extra in ["", "\"romfs_empty\":false,", "\"romfs_empty\":\"true\","] {
            let f = Fixture::new(extra);
            assert!(read(&f.bundle, &f.data).is_err());
            assert!(!f.data.exists());
        }
        let f = Fixture::new("\"romfs_empty\":true,");
        let g = read(&f.bundle, &f.data).unwrap();
        assert_eq!(g.romfs, canonical(&f.data.join("empty-romfs")).unwrap());
        fs::write(g.romfs.join("unexpected"), []).unwrap();
        assert!(read(&f.bundle, &f.data).is_err());
    }
    #[test]
    fn true_rejects_nonempty_bundle_romfs() {
        let f = Fixture::new("\"romfs_empty\":true,");
        fs::create_dir_all(f.bundle.join("nxkit/romfs")).unwrap();
        fs::write(f.bundle.join("nxkit/romfs/file"), []).unwrap();
        assert!(read(&f.bundle, &f.data).is_err());
    }
    #[test]
    fn bad_title_and_asset_paths_are_refused() {
        let f = Fixture::new("\"romfs_empty\":true,");
        for title in ["../save", "01007EF00011E00", "01007EF00011E00G"] {
            f.write(&f.source.replace("01007EF00011E000", title));
            assert!(read(&f.bundle, &f.data).is_err());
        }
        for path in ["/tmp/romfs", "../romfs", "nxkit/../romfs", "C:\\romfs", ""] {
            assert!(relative(Path::new(path)).is_err());
        }
        f.write(
            &f.source
                .replace("Frameworks/nxgame.dylib", "../outside.dylib"),
        );
        assert!(read(&f.bundle, &f.data).is_err());
    }
    #[test]
    fn bundle_cannot_be_writable_root() {
        let f = Fixture::new("\"romfs_empty\":true,");
        assert!(read(&f.bundle, &f.bundle.join("new-data")).is_err());
        assert!(!f.bundle.join("new-data").exists());
    }
    #[test]
    fn font_is_absolute_and_optional_path_types_are_checked() {
        let f = Fixture::new("\"romfs_empty\":true,");
        fs::create_dir_all(f.bundle.join("nxkit")).unwrap();
        fs::write(f.bundle.join("nxkit/font.ttf"), []).unwrap();
        f.write(
            &f.source
                .replace("\"engine_key\": 1", "\"shared_font\": \"nxkit/font.ttf\""),
        );
        let game = read(&f.bundle, &f.data).unwrap();
        assert_eq!(
            PathBuf::from(game.settings.shared_font.unwrap()),
            canonical(&f.bundle.join("nxkit/font.ttf")).unwrap()
        );
        f.write(
            &f.source
                .replace("\"engine_key\": 1", "\"shared_font\": \"../font.ttf\""),
        );
        assert!(read(&f.bundle, &f.data).is_err());
        for key in ["npdm", "patches"] {
            f.write(&f.source.replace(
                "\"romfs_empty\":true",
                &format!("\"romfs_empty\":true,\"{key}\":42"),
            ));
            assert!(read(&f.bundle, &f.data).is_err());
        }
    }
    #[test]
    fn explicit_empty_existing_romfs_is_preserved() {
        let f = Fixture::new("\"romfs_empty\":true,");
        fs::create_dir_all(f.bundle.join("nxkit/romfs")).unwrap();
        let game = read(&f.bundle, &f.data).unwrap();
        assert_eq!(
            game.romfs,
            canonical(&f.bundle.join("nxkit/romfs")).unwrap()
        );
        assert!(!f.data.join("empty-romfs").exists());
    }
    #[cfg(unix)]
    #[test]
    fn missing_romfs_parent_and_empty_replacement_cannot_escape() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new("\"romfs_empty\":true,");
        fs::create_dir_all(f.root.join("outside")).unwrap();
        symlink(f.root.join("outside"), f.bundle.join("outside-link")).unwrap();
        f.write(&f.source.replace("nxkit/romfs", "outside-link/absent"));
        assert!(read(&f.bundle, &f.data).is_err());
        f.write(&f.source);
        fs::create_dir_all(&f.data).unwrap();
        symlink(f.root.join("outside"), f.data.join("empty-romfs")).unwrap();
        assert!(read(&f.bundle, &f.data).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn symlink_escapes_and_nested_escapes_are_refused() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new("\"romfs_empty\":true,");
        fs::create_dir_all(f.root.join("outside")).unwrap();
        fs::create_dir_all(f.bundle.join("nxkit")).unwrap();
        symlink(f.root.join("outside"), f.bundle.join("nxkit/romfs")).unwrap();
        assert!(read(&f.bundle, &f.data).is_err());
        fs::remove_file(f.bundle.join("nxkit/romfs")).unwrap();
        fs::create_dir(f.bundle.join("nxkit/romfs")).unwrap();
        symlink(f.root.join("outside"), f.bundle.join("nxkit/romfs/escape")).unwrap();
        f.write(
            &f.source
                .replace("\"romfs_empty\":true", "\"romfs_empty\":false"),
        );
        assert!(read(&f.bundle, &f.data).is_err());
    }
}
