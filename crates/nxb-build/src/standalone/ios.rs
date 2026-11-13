//! Unsigned iOS packaging over the shared argument, extraction and analysis paths.
use super::{Say, analysis, auto_shaders, extract, generic, guest_tail, ipa, link, stage};
use crate::Args;
use nxb_pyjson::{Value as J, parse};
type Result<T> = std::result::Result<T, String>;

pub(super) fn run(a: &Args, p: &Say) -> Result<()> {
    generic::validate(a).map_err(|e| crate::exit::fail(2, e))?;
    if a.dry_run {
        p.say("auto-analysis: extract packages or NUP, rewrite without executable title patches, translate shaders, package unsigned IPA with the supplied iOS host. Device compatibility is not certified.");
        return Ok(());
    }
    let out = a.out.as_deref().ok_or("missing --out")?;
    generic::vacant(out)?;
    let host = ipa::read_host(a.host_app.as_deref().ok_or("missing --host-app")?)?;
    p.plan(4);
    let game = stage(p, "extract", "validate application metadata", || {
        let game = extract::extract(a, p)?;
        let note = format!("{} [{:016X}] v{}", game.name, game.title_id, game.version);
        Ok((game, note))
    })?;
    let mut files = stage(p, "analysis", "rewrite and link iOS guest", || {
        Ok((
            analysis::ios_runfolder(&game)?,
            "shared analysis and iOS guest image".into(),
        ))
    })?;
    files.extend(stage(
        p,
        "shaders",
        "discover and translate shaders for iOS",
        || {
            Ok((
                auto_shaders::discover(&game.modules, &game.files, true)?,
                "iOS shader libraries".into(),
            ))
        },
    )?);
    files.extend(game.files);
    stage(p, "bundle", "write unsigned IPA", || {
        ipa::write(
            out,
            host,
            files,
            &game.directories,
            game.title_id,
            a.app_name.as_deref().unwrap_or(&game.name),
        )?;
        Ok(((), out.display().to_string()))
    })?;
    p.say(&format!("unsigned IPA: {}", out.display()));
    Ok(())
}
/// Require a new destination, including dangling symlinks.
pub(super) fn vacant(path: &std::path::Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(super::io(path, e)),
        Ok(_) => Err(format!("{} exists: choose a new output", path.display())),
    }
}

pub(super) fn skeleton(path: &std::path::Path) -> Result<std::path::PathBuf> {
    vacant(path)?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| super::io(parent, e))?;
    }
    std::fs::create_dir(path).map_err(|e| super::io(path, e))?;
    Ok(path.to_owned())
}

pub(super) fn manifest(source: &str) -> Result<String> {
    let mut value = parse(source).map_err(|e| format!("manifest: {e:?}"))?;
    value.set("dylib", J::str("Frameworks/nxgame.dylib"));
    Ok(nxb_pyjson::dumps(&value))
}

pub(super) fn write_dylib(
    run: &std::path::Path,
    mods: &[(super::modules::Built, u64)],
    stack_size: u32,
) -> Result<u64> {
    let path = run.join("game.json");
    let source = std::fs::read_to_string(&path).map_err(|e| super::io(&path, e))?;
    let value = parse(&source).map_err(|e| format!("manifest: {e:?}"))?;
    let mut image = link::ios_dylib(mods)?;
    guest_tail::install(&mut image, &value, u64::from(stack_size))?;
    let dir = run.join("Frameworks");
    std::fs::create_dir_all(&dir).map_err(|e| super::io(&dir, e))?;
    let path = dir.join("nxgame.dylib");
    std::fs::write(&path, &image).map_err(|e| super::io(&path, e))?;
    Ok(image.len() as u64)
}

pub(super) fn finish(
    out: &std::path::Path,
    host: &std::path::Path,
    run: &std::path::Path,
    title: u64,
    name: &str,
) -> Result<()> {
    fn visit(
        root: &std::path::Path,
        path: &std::path::Path,
        files: &mut Vec<(String, ipa::Source)>,
        dirs: &mut Vec<String>,
    ) -> Result<()> {
        for entry in std::fs::read_dir(path).map_err(|e| super::io(path, e))? {
            let entry = entry.map_err(|e| super::io(path, e))?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|e| super::io(&path, e))?;
            let name = path
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_str()
                .ok_or("run path is not UTF-8")?
                .replace(std::path::MAIN_SEPARATOR, "/");
            if kind.is_dir() {
                dirs.push(name);
                visit(root, &path, files, dirs)?;
            } else if kind.is_file() {
                files.push((name, ipa::Source::File(path)));
            } else {
                return Err("run folder contains a symbolic link or special file".into());
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    visit(run, run, &mut files, &mut dirs)?;
    ipa::write_sources(out, ipa::read_host(host)?, files, &dirs, title, name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn title_run_folder_matches_memory_packaging() {
        let root = std::env::temp_dir().join(format!("nxb-ios-finish-{}", std::process::id()));
        let host = root.join("host.app");
        let run = root.join("run");
        fs::create_dir_all(&host).unwrap();
        fs::create_dir_all(run.join("romfs/empty")).unwrap();
        let mut image = vec![0u8; 56];
        for (at, n) in [
            (0, 0xfeedfacfu32),
            (4, 0x0100000c),
            (12, 2),
            (16, 1),
            (20, 24),
            (32, 0x32),
            (36, 24),
            (40, 2),
        ] {
            image[at..at + 4].copy_from_slice(&n.to_le_bytes());
        }
        fs::write(host.join("nxkit"), image).unwrap();
        fs::write(host.join("Info.plist"), b"<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>CFBundleExecutable</key><string>nxkit</string></dict></plist>").unwrap();
        let data = vec![0x73; 150_001];
        fs::write(run.join("romfs/data"), &data).unwrap();
        let disk = root.join("disk.ipa");
        let memory = root.join("memory.ipa");
        finish(&disk, &host, &run, 1, "Fixture").unwrap();
        ipa::write(
            &memory,
            ipa::read_host(&host).unwrap(),
            vec![("romfs/data".into(), data)],
            &["romfs".into(), "romfs/empty".into()],
            1,
            "Fixture",
        )
        .unwrap();
        assert_eq!(fs::read(disk).unwrap(), fs::read(memory).unwrap());
        fs::remove_dir_all(root).unwrap();
    }
}
