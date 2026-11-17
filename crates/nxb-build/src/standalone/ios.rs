//! Build the UIKit host and package the shared producer's output for a device.
use crate::host::{Runner, TempDir};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub(super) const BASE: u64 = 0x1_0000_0000;
const TARGET: &str = "aarch64-apple-ios";

pub(super) struct Shell(TempDir);
impl Shell {
    pub(super) fn build() -> Result<Self, String> {
        if !cfg!(target_os = "macos") {
            return Err("building an iOS app requires macOS and Xcode".into());
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .map_err(|e| format!("nxkit source checkout: {e}"))?;
        let source = root.join("tools/ios/shell");
        let cache = root.join("target/ios-shell");
        let shell = Self(TempDir::new_in(&cache)?);
        let mut runner = Runner::from_parent()?;
        for name in ["CARGO_HOME", "RUSTUP_HOME"] {
            if let Some(value) = std::env::var_os(name) {
                runner = runner.with_var(name, value)?;
            }
        }
        let sdk = runner.stdout("xcrun", ["--sdk", "iphoneos", "--show-sdk-path"], None)?;
        let built = runner.run(
            "cargo",
            [
                OsString::from("rustc"),
                "-p".into(),
                "nxkit-host".into(),
                "--lib".into(),
                "--release".into(),
                "--locked".into(),
                "--target".into(),
                TARGET.into(),
                "--target-dir".into(),
                cache.join("rust").into_os_string(),
                "--crate-type".into(),
                "staticlib".into(),
                "--".into(),
                "--print=native-static-libs".into(),
            ],
            Some(&root),
        )?;
        let stderr = String::from_utf8_lossy(&built.stderr);
        let native = stderr
            .lines()
            .find_map(|line| {
                line.split_once("native-static-libs:")
                    .map(|(_, flags)| flags)
            })
            .ok_or("rustc did not report native static libraries")?;
        let mut args: Vec<OsString> = [
            "--sdk",
            "iphoneos",
            "swiftc",
            "-target",
            "arm64-apple-ios26.0",
            "-sdk",
            sdk.trim(),
            "-O",
            "-parse-as-library",
            "-import-objc-header",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        args.push(source.join("Nxkit.h").into_os_string());
        for file in ["App.swift", "Controls.swift"] {
            args.push(source.join(file).into_os_string());
        }
        args.push(
            cache
                .join("rust")
                .join(TARGET)
                .join("release/libnxkit_host.a")
                .into_os_string(),
        );
        // Preserve rustc's native dependency order instead of duplicating the runtime's frameworks.
        for flag in native.split_whitespace() {
            args.extend(["-Xlinker".into(), flag.into()]);
        }
        for framework in ["UIKit", "AVFAudio", "QuartzCore"] {
            args.extend(["-framework".into(), framework.into()]);
        }
        args.extend(["-o".into(), shell.0.path().join("nxkit").into_os_string()]);
        runner.run("xcrun", args, None)?;
        Ok(shell)
    }

    pub(super) fn bundle(&self, stage: &Path, name: &str, id: &str) -> Result<PathBuf, String> {
        let app = stage
            .join("Payload")
            .join(format!("{}.app", super::bundle::file_name(name)));
        std::fs::create_dir_all(&app).map_err(|e| e.to_string())?;
        std::fs::copy(self.0.path().join("nxkit"), app.join("nxkit")).map_err(|e| e.to_string())?;
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
}

pub(super) fn publish(stage: &Path, output: &Path, replace: bool) -> Result<(), String> {
    let archive = std::path::absolute(stage)
        .map_err(|e| e.to_string())?
        .join("archive.ipa");
    if archive.exists() {
        std::fs::remove_file(&archive).map_err(|e| e.to_string())?;
    }
    Runner::from_parent()?.run(
        "/usr/bin/zip",
        [
            OsString::from("-q"),
            "-r".into(),
            "-0".into(),
            archive.as_os_str().into(),
            "Payload".into(),
        ],
        Some(stage),
    )?;
    if replace {
        std::fs::rename(&archive, output).map_err(|e| e.to_string())?;
    } else {
        // The generic producer must not replace an output that appeared during its build.
        std::fs::hard_link(&archive, output).map_err(|e| e.to_string())?;
    }
    std::fs::remove_dir_all(stage).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packages_shell_and_game_without_replacing_an_existing_output() {
        let temp = TempDir::new_in(&std::env::temp_dir()).unwrap();
        let shell = Shell(TempDir::new_in(temp.path()).unwrap());
        std::fs::write(shell.0.path().join("nxkit"), b"host").unwrap();
        let stage = temp.path().join("stage");
        let app = shell.bundle(&stage, "A & B", "app.game").unwrap();
        std::fs::write(app.join("game.json"), b"{}").unwrap();
        let plist = std::fs::read_to_string(app.join("Info.plist")).unwrap();
        assert!(plist.contains("A &amp; B") && plist.contains("UIApplicationSceneManifest"));
        let output = temp.path().join("game.ipa");
        std::fs::write(&output, b"keep").unwrap();
        assert!(publish(&stage, &output, false).is_err());
        assert_eq!(std::fs::read(&output).unwrap(), b"keep");
        std::fs::remove_file(&output).unwrap();
        publish(&stage, &output, false).unwrap();
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
}
