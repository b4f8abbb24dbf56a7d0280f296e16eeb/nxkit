//! Package the existing UIKit shell and one title's data as an unsigned IPA.
use nxb_tool::{Invocation, ProcessRegistry, run_process};
use std::path::{Path, PathBuf};

pub(super) fn shell(dry_run: bool) -> Result<(PathBuf, Option<crate::host::TempDir>), String> {
    let (path, assembly) = match std::env::var_os("NXB_IOS_SHELL") {
        Some(path) => (PathBuf::from(path), None),
        None if dry_run => (PathBuf::from("<iOS shell built from source>"), None),
        None => {
            let (path, assembly) = super::shell::build()?;
            (path, Some(assembly))
        }
    };
    if dry_run {
        return Ok((path, assembly));
    }
    if !path.join("nxkit").is_file() || !path.join("Info.plist").is_file() {
        return Err(format!(
            "{}: expected the iOS shell with nxkit and Info.plist",
            path.display()
        ));
    }
    validate_shell(&path)?;
    Ok((path, assembly))
}

fn validate_shell(path: &Path) -> Result<(), String> {
    let bytes = std::fs::read(path.join("nxkit")).map_err(|e| e.to_string())?;
    let image = nxb_pack::macho::MachO::parse(&bytes).map_err(|e| e.to_string())?;
    if image.cputype != 0x0100_000c
        || image.filetype != 2
        || image.platform != Some(nxb_pack::macho::platform::IOS)
        || image.minos.is_none_or(|minimum| minimum > 26 << 16)
    {
        return Err(
            "NXB_IOS_SHELL must contain an ARM64 iOS device executable for iOS 26 or earlier"
                .into(),
        );
    }
    Ok(())
}

fn copy(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(from).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        if name == "_CodeSignature" || name == "embedded.mobileprovision" {
            continue;
        }
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_dir() {
            copy(&entry.path(), &to.join(name))?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), to.join(name)).map_err(|e| e.to_string())?;
        } else {
            return Err(format!(
                "{}: the shell must contain regular files and folders",
                entry.path().display()
            ));
        }
    }
    Ok(())
}

pub(super) fn skeleton(
    stage: &Path,
    shell: &Path,
    name: &str,
    id: &str,
) -> Result<PathBuf, String> {
    let app = stage
        .join("Payload")
        .join(format!("{}.app", super::bundle::file_name(name)));
    copy(shell, &app)?;
    // The shell executable is supplied separately. Its bundle contains no game.
    if app.join("game.json").exists() {
        return Err("the iOS shell already contains a game".into());
    }
    let plist_path = app.join("Info.plist");
    let source = std::fs::read(&plist_path).map_err(|e| e.to_string())?;
    let mut plist = nxb_pack::plist::parse(&source).map_err(|e| e.to_string())?;
    let nxb_pack::plist::Value::Dict(ref mut entries) = plist else {
        return Err("the shell Info.plist must be a dictionary".into());
    };
    for (key, value) in [
        ("CFBundleExecutable", "nxkit"),
        ("CFBundleIdentifier", id),
        ("CFBundleName", name),
        ("CFBundleDisplayName", name),
        ("MinimumOSVersion", "26.0"),
    ] {
        entries.retain(|(k, _)| k != key);
        entries.push((key.into(), nxb_pack::plist::Value::String(value.into())));
    }
    let plist = nxb_pack::plist::to_cf_xml(&plist).map_err(|e| e.to_string())?;
    std::fs::write(plist_path, plist).map_err(|e| e.to_string())?;
    Ok(app)
}

pub(super) fn finish(stage: &Path, out: &Path, replace: bool) -> Result<(), String> {
    let partial = std::path::absolute(stage)
        .map_err(|e| e.to_string())?
        .join("archive.ipa");
    // zip updates existing archives. Remove only our temporary output first.
    if partial.exists() {
        std::fs::remove_file(&partial).map_err(|e| e.to_string())?;
    }
    let command = Invocation::new("/usr/bin/zip")
        .args(["-q", "-r", "-0"])
        .arg(partial.as_os_str())
        .arg("Payload")
        .cwd(stage);
    run_process("zip", &command, &ProcessRegistry::new()).map_err(|e| e.to_string())?;
    if replace {
        std::fs::rename(&partial, out).map_err(|e| e.to_string())?;
    } else {
        // A generic build never replaces another output, even if it appeared
        // after the initial vacancy check. The staging folder is beside the output.
        std::fs::hard_link(&partial, out).map_err(|e| e.to_string())?;
        std::fs::remove_file(&partial).map_err(|e| e.to_string())?;
    }
    std::fs::remove_dir_all(stage).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dry_run_does_not_need_an_apple_toolchain_or_shell() {
        assert!(shell(true).is_ok());
    }

    #[test]
    fn packages_shell_and_title_without_provisioning_identity() {
        let root = std::env::temp_dir().join(format!("nxb-ipa-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let shell = root.join("shell");
        std::fs::create_dir_all(shell.join("_CodeSignature")).unwrap();
        for file in [
            "nxkit",
            "Info.plist",
            "embedded.mobileprovision",
            "_CodeSignature/CodeResources",
        ] {
            std::fs::write(shell.join(file), b"fixture").unwrap();
        }
        std::fs::write(
            shell.join("Info.plist"),
            b"<plist><dict><key>UIApplicationSceneManifest</key><dict/></dict></plist>",
        )
        .unwrap();
        assert!(validate_shell(&shell).is_err());
        let mut binary = super::super::macho::write_target(
            &[super::super::macho::Seg {
                name: "__CODE",
                addr: 0x1_0001_0000,
                data: &[0xc0, 0x03, 0x5f, 0xd6],
                mem_size: 0x4000,
                exec: true,
            }],
            [0; 16],
            Some(0x4000),
        )
        .unwrap();
        binary[12..16].copy_from_slice(&2u32.to_le_bytes());
        std::fs::write(shell.join("nxkit"), binary).unwrap();
        validate_shell(&shell).unwrap();
        let stage = root.join("test.ipa.partial");
        let app = skeleton(&stage, &shell, "A & B", "app.test.game").unwrap();
        assert!(app.join("nxkit").is_file());
        assert!(!app.join("embedded.mobileprovision").exists());
        assert!(!app.join("_CodeSignature").exists());
        assert!(
            std::fs::read_to_string(app.join("Info.plist"))
                .unwrap()
                .contains("A &amp; B")
        );
        assert!(
            std::fs::read_to_string(app.join("Info.plist"))
                .unwrap()
                .contains("UIApplicationSceneManifest")
        );
        std::fs::write(app.join("game.json"), b"{}").unwrap();
        let out = root.join("test.ipa");
        std::fs::write(&out, b"keep").unwrap();
        assert!(finish(&stage, &out, false).is_err());
        assert_eq!(std::fs::read(&out).unwrap(), b"keep");
        std::fs::remove_file(&out).unwrap();
        finish(&stage, &out, false).unwrap();
        assert!(!stage.exists());
        let listing = run_process(
            "unzip",
            &Invocation::new("/usr/bin/unzip")
                .args(["-Z1"])
                .arg(out.as_os_str()),
            &ProcessRegistry::new(),
        )
        .unwrap();
        let listing = String::from_utf8(listing.stdout).unwrap();
        assert!(listing.contains("Payload/A & B.app/game.json"));
        assert!(listing.contains("Payload/A & B.app/nxkit"));
        assert!(!listing.contains("mobileprovision"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
