//! Assemble the existing UIKit shell with the shared Rust host on a Mac.
use crate::host::{Runner, TempDir};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

const TARGET: &str = "aarch64-apple-ios";

fn native_libs(stderr: &[u8]) -> Result<Vec<OsString>, String> {
    String::from_utf8_lossy(stderr)
        .lines()
        .find_map(|line| {
            line.split_once("native-static-libs:")
                .map(|(_, flags)| flags)
        })
        .map(|flags| flags.split_whitespace().map(OsString::from).collect())
        .ok_or_else(|| "cargo did not report the host's native static libraries".into())
}

pub(super) fn build() -> Result<(PathBuf, TempDir), String> {
    if !cfg!(target_os = "macos") {
        return Err("building the iOS shell requires macOS and Xcode; otherwise set NXB_IOS_SHELL to a prebuilt .app".into());
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let root = source.canonicalize().map_err(|_| {
        "the nxkit source checkout is unavailable; set NXB_IOS_SHELL to a prebuilt .app".to_string()
    })?;
    let shell = root.join("tools/ios/shell");
    if !shell.join("App.swift").is_file() {
        return Err("the nxkit source checkout has no iOS shell; set NXB_IOS_SHELL".into());
    }
    let work = root.join("target/ios-shell");
    let assembly = TempDir::new_in(&work)?;
    let app = assembly.path().join("Nxkit.app");
    std::fs::create_dir_all(&app).map_err(|e| e.to_string())?;
    let mut runner = Runner::from_parent()?;
    for name in ["CARGO_HOME", "RUSTUP_HOME"] {
        if let Some(value) = std::env::var_os(name) {
            runner = runner.with_var(name, value)?;
        }
    }
    let sdk = runner.stdout("xcrun", ["--sdk", "iphoneos", "--show-sdk-path"], None)?;
    let sdk = sdk.trim();
    // Ask rustc for its native link requirements instead of duplicating the
    // frameworks used by the shared runtime. Cargo replays these notes on a hit.
    let compiled = runner.run(
        "cargo",
        [
            "rustc".into(),
            "-p".into(),
            "nxkit-host".into(),
            "--lib".into(),
            "--release".into(),
            "--locked".into(),
            "--target".into(),
            TARGET.into(),
            "--target-dir".into(),
            work.join("rust").into_os_string(),
            "--crate-type".into(),
            "staticlib".into(),
            "--".into(),
            "--print=native-static-libs".into(),
        ],
        Some(&root),
    )?;
    let libs = native_libs(&compiled.stderr)?;
    let overlay = assembly.path().join("Gamepad.o");
    runner.run(
        "xcrun",
        [
            "--sdk".into(),
            "iphoneos".into(),
            "clang".into(),
            "-target".into(),
            "arm64-apple-ios26.0".into(),
            "-isysroot".into(),
            sdk.into(),
            "-fobjc-arc".into(),
            "-O2".into(),
            "-c".into(),
            shell.join("overlay/Gamepad.m").into_os_string(),
            "-o".into(),
            overlay.clone().into_os_string(),
        ],
        None,
    )?;
    let mut args: Vec<OsString> = [
        "--sdk",
        "iphoneos",
        "swiftc",
        "-target",
        "arm64-apple-ios26.0",
        "-sdk",
        sdk,
        "-O",
        "-parse-as-library",
        "-import-objc-header",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    args.push(shell.join("BridgingHeader.h").into_os_string());
    for file in ["App.swift", "MetalView.swift", "GameMenu.swift"] {
        args.push(shell.join(file).into_os_string());
    }
    args.push(overlay.into_os_string());
    args.push(
        work.join("rust")
            .join(TARGET)
            .join("release/libnxkit_host.a")
            .into_os_string(),
    );
    for flag in libs {
        args.extend(["-Xlinker".into(), flag]);
    }
    // The Objective-C overlay is not a Swift module and does not auto-link these.
    for framework in ["UIKit", "GameController", "CoreText"] {
        args.extend(["-framework".into(), framework.into()]);
    }
    args.extend(["-o".into(), app.join("nxkit").into_os_string()]);
    runner.run("xcrun", args, None)?;
    std::fs::copy(root.join("tools/ios/Info.plist"), app.join("Info.plist"))
        .map_err(|e| e.to_string())?;
    Ok((app, assembly))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn takes_native_link_requirements_from_rustc() {
        assert_eq!(
            native_libs(b"note: native-static-libs: -lc++ -framework Metal\n").unwrap(),
            ["-lc++", "-framework", "Metal"].map(OsString::from)
        );
        assert!(native_libs(b"no linker requirements").is_err());
    }
}
