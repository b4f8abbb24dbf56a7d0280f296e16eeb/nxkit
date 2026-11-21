//! Which folders at `-o` a build may replace.

use super::replaceable;
use std::fs;
use std::path::PathBuf;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nxb-replace-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d.join("Game.app")
}

fn file(p: PathBuf) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, b"").unwrap();
}

#[test]
fn a_missing_app_and_an_nxkit_app_are_replaced() {
    let out = dir("nxkit");
    assert!(replaceable(&out).is_ok());
    file(out.join("Contents/MacOS/nxkit-host"));
    assert!(replaceable(&out).is_ok());
    fs::remove_dir_all(out.parent().unwrap()).unwrap();
}

#[test]
fn an_app_of_the_old_engine_is_replaced() {
    // what NX2Apple built with nxnative before the game moved to nxkit (issue 43)
    let out = dir("nxnative");
    file(out.join("Contents/MacOS/nxnative"));
    fs::create_dir_all(out.join("Contents/Resources/dm")).unwrap();
    assert!(replaceable(&out).is_ok());
    fs::remove_dir_all(out.parent().unwrap()).unwrap();
}

#[test]
fn another_folder_is_kept() {
    let out = dir("other");
    fs::create_dir_all(out.join("Contents/MacOS")).unwrap();
    assert!(replaceable(&out).is_err());
    // the old engine's launcher alone, without its dm folder, is not enough
    file(out.join("Contents/MacOS/nxnative"));
    assert!(replaceable(&out).is_err());
    fs::remove_dir_all(out.parent().unwrap()).unwrap();
}
