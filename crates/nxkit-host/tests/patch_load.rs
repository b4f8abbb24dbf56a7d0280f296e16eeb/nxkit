//! Loads the example patch module (`tools/patch-example`) as a real `cdylib`: build, load, call the hooks, replace,
//! unload (W37). The test runs `cargo build` for the example in a target folder of its own.
#![cfg(target_os = "macos")]
#![allow(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use nxkit_patch_abi as abi;
use nxkit_patch_abi::registry;

fn example_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/patch-example")
}

fn build(target: &Path) -> PathBuf {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args(["build", "--release", "--offline", "--manifest-path"])
        .arg(example_dir().join("Cargo.toml"))
        .arg("--target-dir")
        .arg(target)
        .env_remove("CARGO_TARGET_DIR")
        .output();
    let ok = out.as_ref().is_ok_and(|o| o.status.success());
    assert!(
        ok,
        "building the example failed: {}",
        out.map_or_else(
            |e| e.to_string(),
            |o| String::from_utf8_lossy(&o.stderr).into_owned()
        )
    );
    target.join("release/libpatch_example.dylib")
}

fn draw_with_fragment(key: &str) -> abi::DrawInfo {
    abi::DrawInfo {
        programs: abi::ProgramKeys {
            vertex: abi::Key::new("vs"),
            fragment: abi::Key::new(key),
        },
        ..Default::default()
    }
}

#[test]
fn the_example_loads_answers_reloads_and_unloads() {
    let target = std::env::temp_dir().join(format!("nxkit-patch-test-{}", std::process::id()));
    let dylib = build(&target);
    let entry = nxkit_host::sys::patchlib::load(&dylib);
    let Ok(entry) = entry else {
        unreachable!("the load failed: {:?}", entry.err());
    };
    assert_eq!(entry.name, "patch-example");
    assert!(registry::install(entry).is_none());
    assert!(registry::active(registry::HOOK_DRAW | registry::HOOK_PASS));
    // The key in the source is empty, so nothing is skipped.
    assert_eq!(registry::draw(&draw_with_fragment("fs_blocked")).flags, 0);
    assert_eq!(
        registry::pass(&abi::PassInfo::default()),
        abi::PassAnswer::default()
    );
    registry::frame_begin(&abi::FrameBegin::default());
    assert_eq!(registry::list().len(), 1);

    // The same file loaded again is a new image. The patch reads its key from the environment at load. The old patch
    // is replaced in its place.
    // SAFETY: no other thread of this test binary reads or writes the environment.
    unsafe { std::env::set_var("PATCH_EXAMPLE_SKIP_FS", "fs_blocked") };
    let again = nxkit_host::sys::patchlib::load(&dylib);
    let Ok(again) = again else {
        unreachable!("the second load failed: {:?}", again.err());
    };
    drop(registry::install(again));
    assert_eq!(registry::list().len(), 1);
    assert_eq!(
        registry::draw(&draw_with_fragment("fs_blocked")).flags,
        abi::DRAW_SKIP
    );
    assert_eq!(registry::draw(&draw_with_fragment("fs_other")).flags, 0);

    drop(registry::remove(Some("patch-example")));
    assert!(!registry::active(registry::HOOK_DRAW | registry::HOOK_PASS));
    assert_eq!(registry::draw(&draw_with_fragment("fs_blocked")).flags, 0);

    // The copies are gone, and a file that is no patch is refused.
    let folder = nxkit_host::control::folder().join("patches");
    let left = std::fs::read_dir(&folder)
        .map(|d| d.flatten().count())
        .unwrap_or(0);
    assert_eq!(left, 0, "the copies of the library stay in {folder:?}");
    assert!(nxkit_host::sys::patchlib::load(&example_dir().join("Cargo.toml")).is_err());
    let _ = std::fs::remove_dir_all(&target);
}
