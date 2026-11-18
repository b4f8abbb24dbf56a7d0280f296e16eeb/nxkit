//! Package a locally built UIKit host with the shared producer's output for a device.
use crate::host::{Runner, TempDir};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub(super) const BASE: u64 = 0x1_0000_0000;

/// Use the control icon already extracted by either producer. Homebrew may omit it.
pub(super) fn icon(app: &Path, image: &Path) -> Result<(), String> {
    if !image.is_file() {
        return Ok(());
    }
    let temp = TempDir::new_in(&std::env::temp_dir())?;
    let catalog = temp.path().join("Assets.xcassets");
    let icons = catalog.join("AppIcon.appiconset");
    std::fs::create_dir_all(&icons).map_err(|e| e.to_string())?;
    std::fs::write(icons.join("Contents.json"), br#"{"images":[{"filename":"Icon.png","idiom":"universal","platform":"ios","size":"1024x1024"}],"info":{"author":"xcode","version":1}}"#)
        .map_err(|e| e.to_string())?;
    let runner = Runner::from_parent()?;
    runner.run(
        "/usr/bin/sips",
        [
            OsString::from("-s"),
            "format".into(),
            "png".into(),
            "-z".into(),
            "1024".into(),
            "1024".into(),
            image.into(),
            "--out".into(),
            icons.join("Icon.png").into_os_string(),
        ],
        None,
    )?;
    let partial = temp.path().join("icon.plist");
    runner.run(
        "xcrun",
        [
            OsString::from("actool"),
            "--compile".into(),
            app.into(),
            "--platform".into(),
            "iphoneos".into(),
            "--minimum-deployment-target".into(),
            "26.0".into(),
            "--target-device".into(),
            "iphone".into(),
            "--target-device".into(),
            "ipad".into(),
            "--app-icon".into(),
            "AppIcon".into(),
            "--output-partial-info-plist".into(),
            partial.clone().into_os_string(),
            catalog.into_os_string(),
        ],
        None,
    )?;
    let path = app.join("Info.plist");
    let mut plist = nxb_pack::plist::parse(&std::fs::read(&path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let icon = nxb_pack::plist::parse(&std::fs::read(partial).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let (nxb_pack::plist::Value::Dict(entries), nxb_pack::plist::Value::Dict(icon)) =
        (&mut plist, icon)
    else {
        return Err("app icon plist is not a dictionary".into());
    };
    for (key, value) in icon {
        entries.retain(|(k, _)| k != &key);
        entries.push((key, value));
    }
    std::fs::write(
        path,
        nxb_pack::plist::to_cf_xml(&plist).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

pub(super) fn bundle(stage: &Path, host: &Path, name: &str, id: &str) -> Result<PathBuf, String> {
    let app = stage
        .join("Payload")
        .join(format!("{}.app", super::bundle::file_name(name)));
    std::fs::create_dir_all(&app).map_err(|e| e.to_string())?;
    std::fs::copy(host, app.join("nxkit")).map_err(|e| e.to_string())?;
    let mut plist = nxb_pack::plist::parse(include_bytes!("../../../../tools/ios/Info.plist"))
        .map_err(|e| e.to_string())?;
    let nxb_pack::plist::Value::Dict(ref mut entries) = plist else {
        return Err("shell plist is not a dictionary".into());
    };
    for (key, value) in [
        ("CFBundleIdentifier", id),
        ("CFBundleName", name),
        ("CFBundleDisplayName", name),
    ] {
        entries.retain(|(k, _)| k != key);
        entries.push((key.into(), nxb_pack::plist::Value::String(value.into())));
    }
    std::fs::write(
        app.join("Info.plist"),
        nxb_pack::plist::to_cf_xml(&plist).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(app)
}

// Only disposable staging files are normalized. Payload contents and permission bits stay intact.
fn archive_entries(
    stage: &Path,
    relative: &Path,
    entries: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let path = stage.join(relative);
    let metadata = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    if !metadata.is_file() && !metadata.is_dir() {
        return Err("IPA staging contains a non-regular entry".into());
    }
    if metadata.is_dir() {
        for child in std::fs::read_dir(&path).map_err(|e| e.to_string())? {
            let child = child.map_err(|e| e.to_string())?;
            archive_entries(stage, &relative.join(child.file_name()), entries)?;
        }
    }
    // ZIP's DOS timestamp starts at 1980. UTC in the child makes the stored value timezone-independent.
    let epoch = std::time::UNIX_EPOCH + std::time::Duration::from_secs(315_532_800);
    std::fs::File::open(&path)
        .and_then(|file| file.set_times(std::fs::FileTimes::new().set_modified(epoch)))
        .map_err(|e| e.to_string())?;
    entries.push(relative.to_path_buf());
    Ok(())
}

pub(super) fn publish(stage: &Path, output: &Path) -> Result<(), String> {
    let archive = std::path::absolute(stage)
        .map_err(|e| e.to_string())?
        .join("archive.ipa");
    if archive.exists() {
        std::fs::remove_file(&archive).map_err(|e| e.to_string())?;
    }
    let mut entries = Vec::new();
    archive_entries(stage, Path::new("Payload"), &mut entries)?;
    entries.sort();
    let runner = Runner::from_parent()?.with_var("TZ", "UTC")?;
    // Explicit sorted entries avoid filesystem traversal order. -X drops variable extra metadata.
    // Bound argument batches so large bundles do not exceed the host's command-line limit.
    for batch in entries.chunks(32) {
        let mut args: Vec<OsString> = ["-q", "-0", "-X", "-nw"]
            .into_iter()
            .map(OsString::from)
            .collect();
        args.push(archive.as_os_str().into());
        args.extend(batch.iter().map(|p| p.as_os_str().into()));
        runner.run("/usr/bin/zip", args, Some(stage))?;
    }
    // Neither producer may replace an output that appeared during its build.
    std::fs::hard_link(&archive, output).map_err(|e| e.to_string())?;
    std::fs::remove_dir_all(stage).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packages_shell_and_game_without_replacing_an_existing_output() {
        let temp = TempDir::new_in(&std::env::temp_dir()).unwrap();
        let source = temp.path().join("nxkit-host-ios");
        std::fs::write(&source, b"host").unwrap();
        let exe = temp.path().join("nxb");
        let host = super::super::bundle::host(None, &exe, true).unwrap();
        assert_eq!(host, source);
        assert_eq!(
            super::super::bundle::host(Some(""), &exe, true).unwrap(),
            source
        );
        assert_eq!(
            super::super::bundle::host(source.to_str(), &exe, true).unwrap(),
            source
        );
        // An iOS host must not serve as the default Mac host.
        assert!(super::super::bundle::host(None, &exe, false).is_err());
        let stage = temp.path().join("stage");
        let app = bundle(&stage, &host, "A & B", "app.game").unwrap();
        std::fs::write(app.join("game.json"), b"{}").unwrap();
        // Exercise Apple's asset compiler where available, using a generated one-pixel RGB image.
        if cfg!(target_os = "macos")
            && Runner::from_parent()
                .and_then(|runner| {
                    runner.run("xcrun", ["--sdk", "iphoneos", "--find", "actool"], None)
                })
                .is_ok()
        {
            let image = temp.path().join("icon.png");
            std::fs::write(
                &image,
                [
                    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0,
                    0, 1, 8, 2, 0, 0, 0, 144, 119, 83, 222, 0, 0, 0, 12, 73, 68, 65, 84, 120, 156,
                    99, 104, 112, 80, 0, 0, 2, 36, 0, 225, 171, 89, 98, 39, 0, 0, 0, 0, 73, 69, 78,
                    68, 174, 66, 96, 130,
                ],
            )
            .unwrap();
            icon(&app, &image).unwrap();
            assert!(app.join("Assets.car").is_file());
            let info = std::fs::read_to_string(app.join("Info.plist")).unwrap();
            assert!(info.contains("CFBundleIcons") && info.contains("AppIcon"));
        }
        // A second game reuses the same host and has its own bundle identity.
        let second = bundle(&temp.path().join("second"), &host, "Second", "app.second").unwrap();
        assert_eq!(std::fs::read(second.join("nxkit")).unwrap(), b"host");
        assert_eq!(std::fs::read(app.join("nxkit")).unwrap(), b"host");
        assert!(
            std::fs::read_to_string(second.join("Info.plist"))
                .unwrap()
                .contains("app.second")
        );
        assert_eq!(std::fs::read(&host).unwrap(), b"host");
        let plist = std::fs::read_to_string(app.join("Info.plist")).unwrap();
        assert!(plist.contains("A &amp; B") && plist.contains("UIApplicationSceneManifest"));
        let output = temp.path().join("game.ipa");
        std::fs::write(&output, b"keep").unwrap();
        assert!(publish(&stage, &output).is_err());
        assert_eq!(std::fs::read(&output).unwrap(), b"keep");
        std::fs::remove_file(&output).unwrap();
        publish(&stage, &output).unwrap();
        let listing = Runner::from_parent()
            .unwrap()
            .stdout(
                "/usr/bin/unzip",
                [OsString::from("-Z1"), output.into_os_string()],
                None,
            )
            .unwrap();
        assert!(listing.contains("Payload/A & B.app/game.json"));
        assert!(listing.contains("Payload/A & B.app/nxkit"));
        assert!(!stage.exists());
    }

    #[test]
    fn archive_bytes_ignore_creation_order_and_timestamps() {
        let temp = TempDir::new_in(&std::env::temp_dir()).unwrap();
        let mut names: Vec<String> = (0..40).map(|n| format!("file-{n:02}.txt")).collect();
        names.push("[literal].txt".into());
        names.push("nxkit".into());
        let mut archives = Vec::new();
        for reverse in [false, true] {
            let stage = temp.path().join(if reverse { "second" } else { "first" });
            let app = stage.join("Payload/Game.app");
            std::fs::create_dir_all(app.join("empty")).unwrap();
            let mut ordered = names.clone();
            if reverse {
                ordered.reverse();
            }
            for name in ordered {
                let path = app.join(&name);
                std::fs::write(&path, name.as_bytes()).unwrap();
                #[cfg(unix)]
                if name == "nxkit" {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                        .unwrap();
                }
                let time = std::time::UNIX_EPOCH
                    + std::time::Duration::from_secs(if reverse {
                        1_700_000_000
                    } else {
                        1_600_000_000
                    });
                std::fs::File::open(path)
                    .unwrap()
                    .set_times(std::fs::FileTimes::new().set_modified(time))
                    .unwrap();
            }
            let output = temp
                .path()
                .join(if reverse { "second.ipa" } else { "first.ipa" });
            publish(&stage, &output).unwrap();
            archives.push(std::fs::read(&output).unwrap());
            let listing = Runner::from_parent()
                .unwrap()
                .stdout(
                    "/usr/bin/unzip",
                    [OsString::from("-Z1"), output.as_os_str().into()],
                    None,
                )
                .unwrap();
            assert!(listing.contains("Payload/Game.app/empty/"));
            assert!(listing.contains("Payload/Game.app/[literal].txt"));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let extracted = temp
                    .path()
                    .join(if reverse { "unzip-b" } else { "unzip-a" });
                Runner::from_parent()
                    .unwrap()
                    .run(
                        "/usr/bin/unzip",
                        [
                            OsString::from("-q"),
                            output.into_os_string(),
                            "-d".into(),
                            extracted.as_os_str().into(),
                        ],
                        None,
                    )
                    .unwrap();
                let permissions = std::fs::metadata(extracted.join("Payload/Game.app/nxkit"))
                    .unwrap()
                    .permissions();
                assert_eq!(permissions.mode() & 0o777, 0o755);
            }
        }
        assert_eq!(archives[0], archives[1]);
    }
}
