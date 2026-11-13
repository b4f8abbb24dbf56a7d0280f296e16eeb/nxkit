//! `nxb build` as the app binary calls it: events on standard output in json mode, exit codes, argument errors.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::process::Command;

fn build(args: &[&str]) -> (Option<i32>, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_nxb"))
        .arg("build")
        .args(args)
        // any file stands for the host program: these runs stop before it is used
        .env("NXB_NXKIT_HOST", env!("CARGO_BIN_EXE_nxb"))
        .output()
        .expect("run");
    (
        o.status.code(),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nxb-cli-build-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Every standard output line of a json run is one JSON object.
fn assert_ndjson(out: &str) {
    assert!(!out.trim().is_empty());
    for l in out.lines() {
        let v = nxb_pyjson::parse(l).unwrap_or_else(|_| panic!("not JSON: {l}"));
        assert!(v.get("ev").is_some(), "{l}");
    }
}

#[test]
fn json_mode_standard_output_is_events_only() {
    let d = tmp("json");
    let p = d.to_str().unwrap();
    for args in [
        vec![p, "--progress", "json", "--bogus"],
        vec![p, "--progress", "json"],
        vec![p, "--progress=json", "--platform", "nope"],
    ] {
        let (_, out, _) = build(&args);
        assert_ndjson(&out);
    }
}

fn last_msg(out: &str) -> String {
    let l = out.lines().last().unwrap_or_default();
    nxb_pyjson::parse(l)
        .ok()
        .and_then(|v| v.get("msg").and_then(|m| m.as_str().map(str::to_string)))
        .unwrap_or_default()
}

#[test]
fn a_folder_without_a_package_is_a_clear_error() {
    let d = tmp("empty");
    std::fs::write(d.join("notes.txt"), b"x").unwrap();
    let (code, out, _) = build(&[d.to_str().unwrap(), "--progress", "json"]);
    assert_eq!(code, Some(1));
    assert!(last_msg(&out).contains("no game package"), "{out}");
    let (code, out, _) = build(&["/nonexistent-nxb-folder", "--progress", "json"]);
    assert_eq!(code, Some(1));
    assert!(last_msg(&out).contains("is not a folder"), "{out}");
}

#[test]
fn exit_codes_by_class() {
    let d = tmp("codes");
    let p = d.to_str().unwrap();
    // bad arguments 2, a folder with no package 1
    assert_eq!(build(&[p, "--platform", "tv"]).0, Some(2));
    assert_eq!(build(&[p, "--platform", "ios"]).0, Some(2));
    assert_eq!(build(&[p, "--threads"]).0, Some(2));
    assert_eq!(build(&[p, "--platform", "mac"]).0, Some(1));
    // the options of the engine path are gone
    for gone in ["--engine", "--shaders", "--program", "--dump", "--romfs"] {
        assert_eq!(build(&[p, gone, "x"]).0, Some(2), "{gone}");
    }
    // a game without a title module is refused: 4
    let other = "0100000000000000";
    // an empty PFS0 (no files): a package for the folder scan, no content
    let mut pfs0 = vec![0u8; 0x200];
    pfs0[..4].copy_from_slice(b"PFS0");
    std::fs::write(d.join(format!("Game [{other}][v0].nsp")), &pfs0).unwrap();
    assert_eq!(build(&[p, "--progress", "json"]).0, Some(4));
    std::fs::remove_file(d.join(format!("Game [{other}][v0].nsp"))).unwrap();
    // a package of a game with a title module, no keys: 2 without -k, 6 with a missing keys file
    let t = "01007EF00011E000";
    std::fs::write(d.join(format!("Game [{t}][v0].nsp")), &pfs0).unwrap();
    let u = "01007EF00011E800";
    std::fs::write(d.join(format!("Game [{u}][v1114112].nsp")), &pfs0).unwrap();
    let work = d.join("work");
    let base = [
        p,
        "--platform",
        "mac-app",
        "--work-dir",
        work.to_str().unwrap(),
        "--progress",
        "json",
    ];
    assert_eq!(build(&base).0, Some(2));
    let mut with_keys = base.to_vec();
    with_keys.extend(["-k", "/nonexistent-prod.keys"]);
    assert_eq!(build(&with_keys).0, Some(6));
}

#[test]
fn generic_build_reports_missing_keys_with_the_key_exit_code() {
    let d = tmp("generic-keys");
    let input = d.join("game.nsp");
    let keys = d.join("prod.keys");
    std::fs::write(&input, []).unwrap();
    for contents in [None, Some(&b""[..]), Some(&b"\xff"[..])] {
        if let Some(contents) = contents {
            std::fs::write(&keys, contents).unwrap();
        }
        let (code, out, _) = build(&[
            input.to_str().unwrap(),
            "--auto-analyze",
            "--own-front",
            "on",
            "-k",
            keys.to_str().unwrap(),
            "--progress",
            "json",
        ]);
        assert_eq!(code, Some(6), "{out}");
        assert_ndjson(&out);
    }
    std::fs::remove_dir_all(d).unwrap();
}
