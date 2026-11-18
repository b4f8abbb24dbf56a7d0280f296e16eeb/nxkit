//! Store the device payload with stable metadata, including files hard-linked to the extract cache.

use crate::host::Runner;
use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::Path;

fn entry(stage: &Path, path: &Path, manifest: &mut impl Write) -> io::Result<()> {
    let full = stage.join(path);
    let metadata = full.symlink_metadata()?;
    // mtree reads whitespace-delimited fields and backslash escapes. Quote every byte except
    // ordinary path characters. Every name starts with "Payload", away from directive syntax.
    for &byte in path.as_os_str().as_encoded_bytes() {
        if byte.is_ascii_alphanumeric() || b"/._-".contains(&byte) {
            manifest.write_all(&[byte])?;
        } else {
            write!(manifest, "\\{byte:03o}")?;
        }
    }
    if metadata.is_dir() {
        writeln!(manifest, " type=dir mode=0755")?;
        let mut names = fs::read_dir(full)?
            .map(|e| e.map(|e| e.file_name()))
            .collect::<io::Result<Vec<_>>>()?;
        names.sort_unstable();
        for name in names {
            entry(stage, &path.join(name), manifest)?;
        }
    } else if metadata.is_file() {
        // The app executable and its guest dylib are the only program files in the bundle.
        let binary = path.components().count() == 3
            && path
                .file_name()
                .is_some_and(|n| n == "nxkit" || n == "nxgame.dylib");
        let mode = if binary { "0755" } else { "0644" };
        writeln!(manifest, " type=file mode={mode} size={}", metadata.len())?;
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported IPA entry {}", path.display()),
        ));
    }
    Ok(())
}

pub(super) fn write(stage: &Path) -> Result<(), String> {
    let manifest = stage.join("archive.mtree");
    let mut writer =
        BufWriter::new(fs::File::create(&manifest).map_err(|e| super::super::io(&manifest, e))?);
    // 1980-01-01 UTC is the first date representable in a ZIP DOS timestamp.
    writer
        .write_all(b"#mtree\n/set uid=0 gid=0 time=315532800\n")
        .and_then(|()| entry(stage, Path::new("Payload"), &mut writer))
        .and_then(|()| writer.flush())
        .map_err(|e| super::super::io(&manifest, e))?;
    Runner::from_parent()?
        .with_var("TZ", "UTC")?
        .with_var("LC_ALL", "en_US.UTF-8")?
        .run(
            "/usr/bin/bsdtar",
            [
                "-cf",
                "archive.ipa",
                "--format=zip",
                "--options",
                "zip:compression=store",
                "@archive.mtree",
            ],
            Some(stage),
        )?;
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::host::TempDir;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn identical_files_give_identical_archives_and_preserve_source_metadata() {
        let root = TempDir::new_in(&std::env::temp_dir()).unwrap();
        let resource = root.path().join("source");
        fs::write(&resource, b"game content").unwrap();
        let names = ["x #=\\040\n\t", "é", "end ", "@list"];
        let mut archives = Vec::new();
        for n in 0..2 {
            let stage = TempDir::new_in(root.path()).unwrap();
            let app = stage.path().join("Payload/Game.app");
            fs::create_dir_all(app.join("romfs")).unwrap();
            fs::create_dir(app.join("empty")).unwrap();
            let changed = UNIX_EPOCH + Duration::from_secs(1_000_000_000 + n * 30);
            fs::File::open(&resource)
                .unwrap()
                .set_modified(changed)
                .unwrap();
            fs::set_permissions(
                &resource,
                fs::Permissions::from_mode(if n == 0 { 0o600 } else { 0o755 }),
            )
            .unwrap();
            let original = resource.metadata().unwrap();
            let mut order = names;
            if n == 1 {
                order.reverse();
            }
            for name in order {
                fs::hard_link(&resource, app.join("romfs").join(name)).unwrap();
            }
            fs::write(app.join("nxkit"), b"device host").unwrap();
            fs::write(app.join("nxgame.dylib"), b"guest dylib").unwrap();
            write(stage.path()).unwrap();
            let after = resource.metadata().unwrap();
            assert_eq!(after.modified().unwrap(), original.modified().unwrap());
            assert_eq!(after.permissions().mode(), original.permissions().mode());
            let archive = stage.path().join("archive.ipa");
            archives.push(fs::read(&archive).unwrap());
            let unpack = TempDir::new_in(root.path()).unwrap();
            Runner::from_parent()
                .unwrap()
                .with_var("LC_ALL", "en_US.UTF-8")
                .unwrap()
                .run(
                    "/usr/bin/bsdtar",
                    [std::ffi::OsStr::new("-xpf"), archive.as_os_str()],
                    Some(unpack.path()),
                )
                .unwrap();
            let unpacked = unpack.path().join("Payload/Game.app");
            assert!(unpacked.join("empty").is_dir());
            for name in names {
                let path = unpacked.join("romfs").join(name);
                assert_eq!(fs::read(&path).unwrap(), b"game content");
                assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o644);
            }
            for name in ["nxkit", "nxgame.dylib"] {
                assert_eq!(
                    unpacked.join(name).metadata().unwrap().permissions().mode() & 0o777,
                    0o755
                );
            }
        }
        assert_eq!(archives[0], archives[1]);
    }
}
