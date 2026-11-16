//! Hostile names in synthetic packages: an ExeFS name or an HTML RomFS path with `..` or an absolute part is refused
//! before anything is written outside the output folder.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use common::*;
use nxb_extract::Request;

const APP: u64 = 0x0100_0000_0000_1000;

fn request(keys: &std::path::Path, base: &std::path::Path, out: &std::path::Path) -> Request {
    Request {
        keys: keys.to_path_buf(),
        base: base.to_path_buf(),
        out_base: out.to_path_buf(),
        updates: vec![],
        threads: 2,
        mem_limit: 1 << 28,
        cache_chunks: 16,
        digests: None,
        html: true,
        base_outputs: true,
        split: None,
        app: None,
        program: None,
        discard_images: false,
        allow_damaged: false,
        manifest: false,
        progress: None,
        empty_romfs: false,
    }
}

/// A base game NSP: a Program NCA with the ExeFS `exefs` and an HtmlDocument NCA with the RomFS files `manual`.
fn package(exefs: Files, manual: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let rom = vec![("game.bin", vec![1u8; 0x900])];
    let (prog, _) = nca(
        0,
        APP,
        vec![(0, pfs0_section(&exefs)), (1, romfs_section(&rom))],
    );
    let (man, _) = nca(3, APP, vec![(0, romfs_section(manual))]);
    let (pid, mid) = ([7u8; 16], [8u8; 16]);
    let meta = pfs0_section(&[("m.cnmt".into(), cnmt(0x80, APP, 0, &[pid, mid]))]);
    let (m, _) = nca(1, APP, vec![(0, meta)]);
    pfs0(&[
        (format!("{}.nca", hex(&pid)), prog),
        (format!("{}.nca", hex(&mid)), man),
        (format!("{}.cnmt.nca", hex(&[9u8; 16])), m),
    ])
}

fn exefs(extra: &str) -> Files {
    vec![
        ("main".into(), vec![3u8; 0x500]),
        ("main.npdm".into(), npdm(APP)),
        (extra.into(), b"x".to_vec()),
    ]
}

#[test]
fn safe_names_extract() {
    let dir = tmp("hostile-ok");
    let keys = write_keys(&dir);
    let pkg = dir.join("g.nsp");
    std::fs::write(
        &pkg,
        package(exefs("rtld"), &[("index.html", b"<p>".to_vec())]),
    )
    .unwrap();
    nxb_extract::extract(&request(&keys, &pkg, &dir.join("out/o"))).unwrap();
    assert!(dir.join("out/o/ExeFS/rtld").is_file());
    assert!(dir.join("out/o/html/html0/index.html").is_file());
    cleanup(&dir);
}

#[test]
fn hostile_names_are_refused() {
    let dir = tmp("hostile");
    let keys = write_keys(&dir);
    let pkg = dir.join("g.nsp");
    let ok_manual: &[(&str, Vec<u8>)] = &[("index.html", b"<p>".to_vec())];
    for name in ["../evil", "/evil", "..", "a/b"] {
        std::fs::write(&pkg, package(exefs(name), ok_manual)).unwrap();
        let r = nxb_extract::extract(&request(&keys, &pkg, &dir.join("out/o")));
        assert!(
            r.is_err_and(|e| e.to_string().contains("unsafe ExeFS")),
            "ExeFS name {name:?}"
        );
        assert!(!dir.join("out/evil").exists() && !dir.join("out/o/evil").exists());
    }
    for name in ["..", "../../evil", "a/../../../evil"] {
        std::fs::write(&pkg, package(exefs("rtld"), &[(name, b"x".to_vec())])).unwrap();
        let r = nxb_extract::extract(&request(&keys, &pkg, &dir.join("out/o")));
        assert!(
            r.is_err_and(|e| e.to_string().contains("unsafe RomFS")),
            "RomFS path {name:?}"
        );
        assert!(!dir.join("out/evil").exists() && !dir.join("out/o/evil").exists());
    }
    cleanup(&dir);
}
