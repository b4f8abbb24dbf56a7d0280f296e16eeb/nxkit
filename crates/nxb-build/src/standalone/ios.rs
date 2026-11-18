//! Package a locally built UIKit host with the shared producer's output for a device.
use crate::host::{Runner, TempDir};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub(super) const BASE: u64 = 0x1_0000_0000;

/// Keep all intermediate files on the destination volume, owned by this build.
pub(super) fn staging(out: &Path) -> Result<TempDir, String> {
    TempDir::new_in(out.parent().unwrap_or(Path::new(".")))
}

pub(super) fn validate_host(path: &Path) -> Result<(), String> {
    use std::io::Read;
    const CPU_TYPE_ARM64: u32 = 0x0100_000c;
    // The parser only needs the Mach-O header and load commands. The host may be large.
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut header = [0u8; 32];
    file.read_exact(&mut header).map_err(|e| e.to_string())?;
    if header[..4] != 0xfeed_facfu32.to_le_bytes() {
        return Err("expected a thin arm64 iOS device executable".into());
    }
    let commands = u32::from_le_bytes([header[20], header[21], header[22], header[23]]);
    let mut bytes = header.to_vec();
    file.take(u64::from(commands))
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let image = nxb_pack::macho::MachO::parse(&bytes).map_err(|e| e.to_string())?;
    if image.cputype == CPU_TYPE_ARM64
        && image.is_main_binary()
        && image.platform == Some(nxb_pack::macho::platform::IOS)
    {
        Ok(())
    } else {
        Err("expected an arm64 iOS device executable".into())
    }
}

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
    let executable = app.join("nxkit");
    std::fs::copy(host, &executable).map_err(|e| super::io(host, e))?;
    // A source rebuild may have replaced the host since selection. Verify the actual payload.
    validate_host(&executable).map_err(|e| {
        format!(
            "{} (copied from {}): {e}",
            executable.display(),
            host.display()
        )
    })?;
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

pub(super) fn publish(stage: &Path, output: &Path) -> Result<(), String> {
    let archive = stage.join("archive.ipa");
    archive::write(stage)?;
    nxb_sys::publish::file_new(&archive, output).map_err(|e| super::io(output, e))
}

mod archive;

#[cfg(test)]
mod tests {
    use super::*;
    // A thin Mach-O header with __LINKEDIT and LC_BUILD_VERSION, sufficient to check the target.
    fn host_image(cpu: u32, filetype: u32, platform: u32) -> Vec<u8> {
        let mut bytes: Vec<u8> = [0xfeed_facf, cpu, 0, filetype, 2, 96, 0, 0, 0x19, 72]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        bytes.extend_from_slice(b"__LINKEDIT");
        bytes.resize(104, 0);
        bytes.extend(
            [0x32, 24, platform, 26 << 16, 26 << 16, 0]
                .into_iter()
                .flat_map(u32::to_le_bytes),
        );
        bytes
    }

    #[test]
    fn accepts_only_a_device_host_executable() {
        use nxb_pack::macho::platform::{IOS, IOSSIMULATOR, MACOS};
        let temp = TempDir::new_in(&std::env::temp_dir()).unwrap();
        let exe = temp.path().join("nxb");
        let source = temp.path().join("nxkit-host-ios");
        for wrong in [
            host_image(0x0100_000c, 2, MACOS),
            host_image(0x0100_000c, 2, IOSSIMULATOR),
            host_image(0x0100_000c, 6, IOS),
            host_image(0x0100_0007, 2, IOS),
            b"not a Mach-O".to_vec(),
        ] {
            std::fs::write(&source, wrong).unwrap();
            for override_path in [None, source.to_str()] {
                let error = super::super::bundle::host(override_path, &exe, true).unwrap_err();
                assert!(error.contains("build-host.sh"), "{error}");
            }
        }
    }

    #[test]
    fn rejects_a_host_replaced_after_selection() {
        use nxb_pack::macho::platform::{IOS, MACOS};
        let temp = TempDir::new_in(&std::env::temp_dir()).unwrap();
        let override_path = temp.path().join("host");
        std::fs::write(&override_path, host_image(0x0100_000c, 2, IOS)).unwrap();
        let host =
            super::super::bundle::host(override_path.to_str(), &temp.path().join("nxb"), true)
                .unwrap();
        std::fs::write(&override_path, host_image(0x0100_000c, 2, MACOS)).unwrap();
        let error = bundle(&temp.path().join("stage"), &host, "Game", "app.game").unwrap_err();
        assert!(
            error.contains("expected an arm64 iOS device executable"),
            "{error}"
        );
    }

    #[test]
    fn packages_shell_and_game_without_replacing_an_existing_output() {
        let temp = TempDir::new_in(&std::env::temp_dir()).unwrap();
        let source = temp.path().join("nxkit-host-ios");
        let image = host_image(0x0100_000c, 2, nxb_pack::macho::platform::IOS);
        std::fs::write(&source, &image).unwrap();
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
        let output = temp.path().join("new/output/game.ipa");
        let stage = staging(&output).unwrap();
        let stage = stage.path();
        let app = bundle(stage, &host, "A & B", "app.game").unwrap();
        std::fs::write(app.join("game.json"), b"{}").unwrap();
        // Exercise Apple's asset compiler where available, using a generated one-pixel RGB image.
        if cfg!(target_os = "macos")
            && std::process::Command::new("xcrun")
                .args(["--sdk", "iphoneos", "--find", "actool"])
                .output()
                .is_ok_and(|out| out.status.success())
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
        assert_eq!(std::fs::read(second.join("nxkit")).unwrap(), image);
        assert_eq!(std::fs::read(app.join("nxkit")).unwrap(), image);
        assert!(
            std::fs::read_to_string(second.join("Info.plist"))
                .unwrap()
                .contains("app.second")
        );
        assert_eq!(std::fs::read(&host).unwrap(), image);
        let plist = std::fs::read_to_string(app.join("Info.plist")).unwrap();
        assert!(plist.contains("A &amp; B") && plist.contains("UIApplicationSceneManifest"));
        // Host selection and app layout also run on other hosts. The archive tools ship on macOS.
        if !cfg!(target_os = "macos")
            && ["/usr/bin/bsdtar", "/usr/bin/unzip"]
                .iter()
                .any(|tool| !Path::new(tool).is_file())
        {
            eprintln!("skipping IPA archive checks: bsdtar and unzip are required");
            return;
        }
        // An output created after the title-rule producer checked the path must survive.
        std::fs::write(&output, b"keep").unwrap();
        assert!(super::super::bundle::publish(stage, &output, true, true).is_err());
        assert_eq!(std::fs::read(&output).unwrap(), b"keep");
        std::fs::remove_file(&output).unwrap();
        publish(stage, &output).unwrap();
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
    }
}
