//! The app bundle of the standalone build: `Info.plist`, `Contents/MacOS/nxkit-host` as the main program, and the
//! ad hoc signatures (R21).

use std::path::{Path, PathBuf};
use std::process::Command;

/// Publish a completed app, preserving each producer's signing and replacement policy.
pub(super) fn publish(stage: &Path, out: &Path, ios: bool, replace: bool) -> Result<(), String> {
    if ios {
        return super::ios::publish(stage, out);
    }
    // The generic producer signs the whole app; title builds retain their existing signatures.
    if !replace {
        sign(stage)?;
    }
    publish_app(stage, out, replace)
}

pub(super) fn publish_app(stage: &Path, out: &Path, replace: bool) -> Result<(), String> {
    if replace {
        let _ = std::fs::remove_dir_all(out);
    } else {
        // Reserve the destination atomically, without replacing another app or its saves.
        std::fs::create_dir(out).map_err(|e| super::io(out, e))?;
    }
    if let Err(e) = std::fs::rename(stage, out) {
        if !replace {
            let _ = std::fs::remove_dir(out);
        }
        return Err(super::io(out, e));
    }
    Ok(())
}

// symlink_metadata also sees dangling symlinks, which must never be followed for output.
pub(super) fn vacant(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(super::io(path, e)),
        Ok(_) => Err(format!("{} exists: choose a new output", path.display())),
    }
}

const HOST: &str = "nxkit-host";

/// The local host: `NXB_NXKIT_HOST` or the platform's program beside `nxb`.
/// Rebuild it after source or toolchain changes, as with `cargo build -p nxkit-host` on Mac.
pub(crate) fn host(env: Option<&str>, exe: &Path, ios: bool) -> Result<PathBuf, String> {
    let (name, build) = if ios {
        ("nxkit-host-ios", "sh tools/ios/build-host.sh")
    } else {
        (HOST, "cargo build -p nxkit-host")
    };
    let path = match env.filter(|e| !e.is_empty()) {
        Some(e) => PathBuf::from(e),
        None => exe.parent().unwrap_or(Path::new(".")).join(name),
    };
    if path.is_file() {
        return Ok(path);
    }
    Err(format!(
        "{}: the app needs the {name} program and it is missing (build it with `{build}`, or set NXB_NXKIT_HOST)",
        path.display()
    ))
}

/// A name that is safe as a file name: no `/` and no `:`, which Finder shows as one another.
pub(crate) fn file_name(title: &str) -> String {
    let n: String = title
        .chars()
        .filter(|c| !matches!(c, '/' | ':' | '\\' | '\0') && !c.is_control())
        .collect();
    let n = n.trim().trim_start_matches('.').trim().to_string();
    if n.is_empty() { "Game".to_string() } else { n }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The bundle identifier of a title when `--bundle-id` is not given.
pub(crate) fn default_bundle_id(title_id: u64) -> String {
    format!("org.nxkit.game.{title_id:016x}")
}

/// `Info.plist`.
pub(crate) fn info_plist(name: &str, bundle_id: &str, version: &str) -> String {
    let rows = [
        ("CFBundleExecutable", HOST),
        ("CFBundleIdentifier", bundle_id),
        ("CFBundleName", name),
        ("CFBundleDisplayName", name),
        ("CFBundlePackageType", "APPL"),
        ("CFBundleInfoDictionaryVersion", "6.0"),
        ("CFBundleVersion", version),
        ("CFBundleShortVersionString", version),
        ("LSMinimumSystemVersion", "14.0"),
        ("LSApplicationCategoryType", "public.app-category.games"),
    ];
    let mut s = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n",
    );
    for (k, v) in rows {
        s.push_str(&format!(
            "\t<key>{k}</key>\n\t<string>{}</string>\n",
            escape(v)
        ));
    }
    s.push_str("\t<key>NSHighResolutionCapable</key>\n\t<true/>\n</dict>\n</plist>\n");
    s
}

/// Signs `path` ad hoc (`codesign --force --sign -`). The Mac runs no unsigned arm64 code.
pub(crate) fn sign(path: &Path) -> Result<(), String> {
    let o = Command::new("codesign")
        .args(["--force", "--sign", "-"])
        .arg(path)
        .output()
        .map_err(|e| format!("codesign: {e}"))?;
    if o.status.success() {
        return Ok(());
    }
    Err(format!(
        "codesign {}: {}",
        path.display(),
        String::from_utf8_lossy(&o.stderr).trim()
    ))
}

/// Lays out `Info.plist` and `MacOS/nxkit-host` in the bundle `app` and signs the program. Returns the run folder,
/// `Contents/Resources/nxkit`.
pub(crate) fn skeleton(app: &Path, host: &Path, plist: &str) -> Result<PathBuf, String> {
    let (contents, run) = (app.join("Contents"), app.join("Contents/Resources/nxkit"));
    let macos = contents.join("MacOS");
    for d in [&macos, &run] {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    std::fs::write(contents.join("Info.plist"), plist)
        .map_err(|e| format!("{}: {e}", contents.join("Info.plist").display()))?;
    let to = macos.join(HOST);
    std::fs::copy(host, &to).map_err(|e| format!("{}: {e}", host.display()))?;
    sign(&to)?;
    Ok(run)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_name_loses_slashes_and_colons() {
        assert_eq!(file_name("Some Game: The Sequel"), "Some Game The Sequel");
        assert_eq!(file_name("../x/y"), "xy");
        assert_eq!(file_name(":"), "Game");
    }

    #[test]
    fn the_plist_names_the_host_and_escapes() {
        let p = info_plist("A & B", "org.nxkit.game.1", "1114112");
        assert!(p.contains("<key>CFBundleExecutable</key>\n\t<string>nxkit-host</string>"));
        assert!(p.contains("A &amp; B"));
    }

    #[test]
    fn a_missing_host_is_named() {
        let e = host(None, Path::new("/nonexistent/nxb"), false).unwrap_err();
        assert!(e.contains("NXB_NXKIT_HOST"));
        let e = host(None, Path::new("/nonexistent/nxb"), true).unwrap_err();
        assert!(e.contains("nxkit-host-ios") && e.contains("sh tools/ios/build-host.sh"));
        assert!(host(Some("/nonexistent/h"), Path::new("/x/nxb"), false).is_err());
    }
}
