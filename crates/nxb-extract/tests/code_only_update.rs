//! Encrypted code-only updates can replace an unpacked NUP's code without guessing original NCA offsets.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::Path;

use common::*;
use nxb_extract::{ExtractError, Request, extract_code_only_update};

const APP: u64 = 0x0100_0000_0000_2000;
const VERSION: u32 = 65536;
const PROGRAM: [u8; 16] = [1; 16];
const CONTROL: [u8; 16] = [2; 16];
const META: [u8; 16] = [3; 16];

fn program(app: u64, npdm_app: u64, data: bool) -> (Vec<u8>, Vec<Plain>) {
    let code = pfs0_section(&[
        ("main".into(), b"NSO0new code".to_vec()),
        ("main.npdm".into(), npdm(npdm_app)),
    ]);
    let mut sections = vec![(0, code)];
    if data {
        sections.push((1, romfs_section(&[("new.bin", vec![7; 32])])));
    }
    nca(0, app, sections)
}

fn control(icon: bool) -> Vec<u8> {
    let mut files = vec![("control.nacp", vec![4; 0x4000])];
    if icon {
        files.push(("icon_AmericanEnglish.dat", b"new icon".to_vec()));
    }
    nca(2, APP, vec![(0, romfs_section(&files))]).0
}

fn package(kind: u8, app: u64, version: u32, contents: &[[u8; 16]], mut files: Files) -> Vec<u8> {
    let meta = pfs0_section(&[(
        "patch.cnmt".into(),
        cnmt(kind, app | 0x800, version, contents),
    )]);
    files.push((
        format!("{}.cnmt.nca", hex(&META)),
        nca(1, app | 0x800, vec![(0, meta)]).0,
    ));
    pfs0(&files)
}

fn named(id: &[u8; 16], bytes: Vec<u8>) -> (String, Vec<u8>) {
    (format!("{}.nca", hex(id)), bytes)
}

fn request(dir: &Path, bytes: &[u8]) -> Request {
    let base = dir.join("patch.nsp");
    std::fs::write(&base, bytes).unwrap();
    Request {
        keys: write_keys(dir),
        base,
        out_base: dir.join("out"),
        app: Some(APP),
        cache_chunks: 8,
        ..Request::default()
    }
}

fn prepare_base(out: &Path) {
    std::fs::create_dir_all(out.join("ExeFS")).unwrap();
    std::fs::create_dir_all(out.join("RomFS")).unwrap();
    for (name, bytes) in [
        ("ExeFS/main", b"old code".as_slice()),
        ("ExeFS/subsdk9", b"stale module"),
        ("control.nacp", b"old control"),
        ("icon.jpg", b"old icon"),
        ("RomFS/unchanged.bin", b"original RomFS"),
    ] {
        std::fs::write(out.join(name), bytes).unwrap();
    }
}

fn assert_base(out: &Path) {
    assert_eq!(std::fs::read(out.join("ExeFS/main")).unwrap(), b"old code");
    assert_eq!(
        std::fs::read(out.join("ExeFS/subsdk9")).unwrap(),
        b"stale module"
    );
    assert_eq!(
        std::fs::read(out.join("control.nacp")).unwrap(),
        b"old control"
    );
    assert_eq!(std::fs::read(out.join("icon.jpg")).unwrap(), b"old icon");
    assert_eq!(std::fs::read_dir(out).unwrap().count(), 4);
}

#[test]
fn replaces_whole_exefs_preserving_loose_romfs_and_absent_update_control() {
    let dir = tmp("code-only-replaces");
    let bytes = package(
        0x81,
        APP,
        VERSION,
        &[PROGRAM],
        vec![named(&PROGRAM, program(APP, APP, false).0)],
    );
    let req = request(&dir, &bytes);
    prepare_base(&req.out_base);
    extract_code_only_update(&req, VERSION).unwrap();
    assert_eq!(
        std::fs::read(req.out_base.join("ExeFS/main")).unwrap(),
        b"NSO0new code"
    );
    assert_eq!(
        std::fs::read(req.out_base.join("ExeFS/main.npdm")).unwrap(),
        npdm(APP)
    );
    assert!(!req.out_base.join("ExeFS/subsdk9").exists());
    assert_eq!(
        std::fs::read(req.out_base.join("control.nacp")).unwrap(),
        b"old control"
    );
    assert_eq!(
        std::fs::read(req.out_base.join("icon.jpg")).unwrap(),
        b"old icon"
    );
    assert_eq!(
        std::fs::read(req.out_base.join("RomFS/unchanged.bin")).unwrap(),
        b"original RomFS"
    );
    assert_eq!(std::fs::read(&req.base).unwrap(), bytes);
    assert_eq!(std::fs::read_dir(&req.out_base).unwrap().count(), 4);
    cleanup(&dir);
}

#[test]
fn update_control_replaces_base_control_and_removes_stale_icon_when_absent() {
    let dir = tmp("code-only-control");
    for icon in [true, false] {
        let bytes = package(
            0x81,
            APP,
            VERSION,
            &[PROGRAM, CONTROL],
            vec![
                named(&PROGRAM, program(APP, APP, false).0),
                named(&CONTROL, control(icon)),
            ],
        );
        let req = request(&dir, &bytes);
        prepare_base(&req.out_base);
        extract_code_only_update(&req, VERSION).unwrap();
        assert_eq!(
            std::fs::read(req.out_base.join("control.nacp")).unwrap(),
            vec![4; 0x4000]
        );
        if icon {
            assert_eq!(
                std::fs::read(req.out_base.join("icon.jpg")).unwrap(),
                b"new icon"
            );
        } else {
            assert!(!req.out_base.join("icon.jpg").exists());
        }
    }
    cleanup(&dir);
}

#[test]
fn refuses_data_section_without_touching_existing_base() {
    let dir = tmp("code-only-data");
    let bytes = package(
        0x81,
        APP,
        VERSION,
        &[PROGRAM],
        vec![named(&PROGRAM, program(APP, APP, true).0)],
    );
    let req = request(&dir, &bytes);
    prepare_base(&req.out_base);
    let err = extract_code_only_update(&req, VERSION).unwrap_err();
    assert!(matches!(err, ExtractError::Unsupported(_)), "{err}");
    assert!(err.to_string().contains("original base NCA"), "{err}");
    assert_base(&req.out_base);
    cleanup(&dir);
}

#[test]
fn requires_exact_patch_title_version_membership_and_program() {
    let dir = tmp("code-only-metadata");
    let normal = vec![named(&PROGRAM, program(APP, APP, false).0)];
    let packages = [
        package(0x80, APP, VERSION, &[PROGRAM], normal.clone()),
        package(0x81, APP + 0x1000, VERSION, &[PROGRAM], normal.clone()),
        package(0x81, APP, VERSION + 1, &[PROGRAM], normal.clone()),
        package(0x81, APP, VERSION, &[], normal.clone()),
        package(0x81, APP, VERSION, &[CONTROL], normal.clone()),
        package(0x81, APP, VERSION, &[PROGRAM, CONTROL], normal.clone()),
        package(0x81, APP, VERSION, &[PROGRAM, PROGRAM], normal.clone()),
        package(
            0x81,
            APP,
            VERSION,
            &[PROGRAM],
            vec![named(&PROGRAM, program(APP + 1, APP, false).0)],
        ),
        package(
            0x81,
            APP,
            VERSION,
            &[PROGRAM],
            [normal.clone(), normal].concat(),
        ),
    ];
    for bytes in packages {
        let req = request(&dir, &bytes);
        prepare_base(&req.out_base);
        assert!(extract_code_only_update(&req, VERSION).is_err());
        assert_base(&req.out_base);
    }
    cleanup(&dir);
}

#[test]
fn refuses_npdm_mismatch_and_damaged_control_without_changing_base() {
    let dir = tmp("code-only-validate");
    let mut damaged = control(true);
    *damaged.last_mut().unwrap() ^= 1;
    for (npdm_app, ctrl) in [(APP + 1, control(true)), (APP, damaged)] {
        let bytes = package(
            0x81,
            APP,
            VERSION,
            &[PROGRAM, CONTROL],
            vec![
                named(&PROGRAM, program(APP, npdm_app, false).0),
                named(&CONTROL, ctrl),
            ],
        );
        let req = request(&dir, &bytes);
        prepare_base(&req.out_base);
        assert!(extract_code_only_update(&req, VERSION).is_err());
        assert_base(&req.out_base);
    }
    cleanup(&dir);
}

#[test]
fn ignores_unlisted_programs_in_merged_packages() {
    let dir = tmp("code-only-merged");
    let bytes = package(
        0x81,
        APP,
        VERSION,
        &[PROGRAM],
        vec![
            named(&PROGRAM, program(APP, APP, false).0),
            named(&CONTROL, program(APP, APP, true).0),
        ],
    );
    let req = request(&dir, &bytes);
    prepare_base(&req.out_base);
    extract_code_only_update(&req, VERSION).unwrap();
    assert_eq!(
        std::fs::read(req.out_base.join("ExeFS/main")).unwrap(),
        b"NSO0new code"
    );
    cleanup(&dir);
}

#[test]
fn shared_package_reader_accepts_code_only_ncz() {
    let dir = tmp("code-only-ncz");
    let (nca, plain) = program(APP, APP, false);
    let mut ncz = nca[..0x4000].to_vec();
    ncz.extend_from_slice(b"NCZSECTN");
    ncz.extend_from_slice(&(plain.len() as u64).to_le_bytes());
    let mut body = Vec::new();
    for (start, bytes, fs) in plain {
        ncz.extend_from_slice(&start.to_le_bytes());
        ncz.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        ncz.extend_from_slice(&3u64.to_le_bytes());
        ncz.extend_from_slice(&0u64.to_le_bytes());
        ncz.extend_from_slice(&CTR_KEY);
        ncz.extend(fs[0x140..0x148].iter().rev());
        ncz.extend_from_slice(&[0; 8]);
        body.extend(bytes);
    }
    // A zstd frame with one raw final block. No external compressor is needed.
    ncz.extend_from_slice(&[0x28, 0xb5, 0x2f, 0xfd, 0x00, 0x58]);
    ncz.extend_from_slice(&(((body.len() as u32) << 3) | 1).to_le_bytes()[..3]);
    ncz.extend(body);
    let bytes = package(
        0x81,
        APP,
        VERSION,
        &[PROGRAM],
        vec![(format!("{}.ncz", hex(&PROGRAM)), ncz)],
    );
    let req = request(&dir, &bytes);
    prepare_base(&req.out_base);
    extract_code_only_update(&req, VERSION).unwrap();
    assert_eq!(
        std::fs::read(req.out_base.join("ExeFS/main")).unwrap(),
        b"NSO0new code"
    );
    cleanup(&dir);
}
