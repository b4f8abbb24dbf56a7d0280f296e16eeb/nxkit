//! Each `nxb` stage command keeps the exit codes of the reference command line on small inputs.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

fn nxb() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_nxb"))
}

fn run(exe: &Path, args: &[&str]) -> (Option<i32>, String, String) {
    let o = Command::new(exe).args(args).output().expect("run");
    (
        o.status.code(),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

/// `nxb <sub> <args>`.
fn stage(sub: &str, args: &[&str]) -> (Option<i32>, String, String) {
    let mut a = vec![sub];
    a.extend(args);
    run(&nxb(), &a)
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nxb-cli-stage-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn usage_and_failures_match() {
    let t = tmp("fail");
    let t = t.to_str().unwrap();
    let nope = format!("{t}/missing.tsv");
    assert_eq!(stage("split", &[]).0, Some(2));
    let r = stage(
        "split",
        &[
            "--game",
            &nope,
            "--pkg",
            t,
            "--map",
            t,
            "--facts",
            t,
            "--pairs",
            t,
            "--pso-pairs",
            t,
            t,
        ],
    );
    assert_ne!(r.0, Some(0));
    assert_eq!(stage("archive", &[]).0, Some(2));
    assert_eq!(stage("pso", &[]).0, Some(1));
    assert_ne!(stage("sign", &[]).0, Some(0));
    assert_ne!(stage("sign", &["cdhash", &nope]).0, Some(0));
    assert_eq!(stage("extract", &[]).0, Some(2));
    let _ = std::fs::remove_dir_all(t);
}

#[test]
fn airc_is_a_command() {
    assert_eq!(stage("airc", &[]).0, Some(2));
    let t = tmp("airc");
    let src = t.join("k.metal");
    std::fs::write(
        &src,
        "kernel void k(device float* a [[buffer(0)]], uint i [[thread_position_in_grid]]) { a[i] = 1.0; }\n",
    )
    .unwrap();
    let ll = t.join("k.ll");
    let r = stage(
        "airc",
        &["emit", src.to_str().unwrap(), "-o", ll.to_str().unwrap()],
    );
    assert_eq!(r.0, Some(0), "{}", r.2);
    assert!(std::fs::read_to_string(&ll).unwrap().contains("define"));
    let _ = std::fs::remove_dir_all(&t);
}

/// The real Mac shader stage, with a synthetic Maxwell compute shader that only exits.
#[cfg(target_os = "macos")]
#[test]
fn full_shader_build_stamps_the_library() {
    if !Command::new("/usr/bin/xcrun")
        .args(["--sdk", "macosx", "metal", "--version"])
        .output()
        .is_ok_and(|o| o.status.success())
    {
        return;
    }
    let d = tmp("library-stamp");
    let key = "0123456789abcdef";
    let mut record = vec![0; 0x38];
    record[..8].copy_from_slice(b"BTCDBTCD");
    for word in [0_u64, 0xe300_0000_0007_000f, 0, 0] {
        record.extend(word.to_le_bytes());
    }
    record.extend([0x34, 0x12, 0x76, 0x98]);
    record.resize(record.len() + 0x800, 0);
    std::fs::write(d.join("cs_test.bin"), record).unwrap();
    std::fs::write(
        d.join("shader_map.tsv"),
        format!("cs_test.bin\t0\t0\t{key}\t0\n"),
    )
    .unwrap();
    std::fs::write(
        d.join("shader_facts.tsv"),
        format!("{key}\t{{\"local\":[1,1,1]}}\n"),
    )
    .unwrap();
    let run = |keys: &[&str]| {
        let result = Command::new(nxb())
            .current_dir(&d)
            .args(["shaders", "build", ".", "cache", "--profile", "."])
            .args([
                "--game",
                "test-game",
                "--chain",
                "test",
                "--inputs",
                "test-inputs",
            ])
            .args(["--store", "store", "--work", "work", "--scratch", "scratch"])
            .args(["--own-front", "off", "--jobs", "1"])
            .args(keys)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    };
    run(&[]);
    let binary = std::fs::read(d.join(format!("cache/b-test/{key}_b.metallib"))).unwrap();
    assert!(binary.starts_with(b"MTLB"));
    assert!(d.join("cache/b-test/inputs.tsv").is_file());
    run(&[key]);
    assert!(d.join(format!("cache/b-test/{key}_b.metallib")).is_file());
    let _ = std::fs::remove_dir_all(d);
}
