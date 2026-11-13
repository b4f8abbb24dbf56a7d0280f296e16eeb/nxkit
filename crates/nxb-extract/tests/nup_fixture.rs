//! NUP reading on small synthetic containers built here (no game data): layout, image contents, control data, DLC,
//! a standalone update, and the checks on damaged or hostile containers.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use nxb_extract::nup::{NupRequest, extract_nup};

#[path = "common/nup.rs"]
mod nup;
use nup::{Node, Pkg, build};

fn npdm(program: u64) -> Vec<u8> {
    let mut d = vec![0u8; 0x200];
    d[..4].copy_from_slice(b"META");
    d[0x70..0x74].copy_from_slice(&0x100u32.to_le_bytes());
    d[0x74..0x78].copy_from_slice(&0x40u32.to_le_bytes());
    d[0x100..0x104].copy_from_slice(b"ACI0");
    d[0x110..0x118].copy_from_slice(&program.to_le_bytes());
    d
}

#[test]
fn npdm_identity_requires_complete_headers_and_aci0_range() {
    let valid = npdm(42);
    let read = |bytes: &[u8]| nxb_extract::nup::program_id(bytes, "fixture/main.npdm");
    assert_eq!(read(&valid).unwrap(), 42);
    for length in [0, 4, 0x77, 0x7f, 0x117, 0x13f] {
        assert!(read(&valid[..length]).is_err(), "length {length}");
    }
    // The identity fits, but the declared ACI0 section does not.
    let mut oversized = valid.clone();
    oversized[0x74..0x78].copy_from_slice(&0x101u32.to_le_bytes());
    assert!(read(&oversized).is_err());
    for (offset, size) in [(u32::MAX, 0x40), (0x100, u32::MAX), (0x100, 0x17)] {
        let mut bad = valid.clone();
        bad[0x70..0x74].copy_from_slice(&offset.to_le_bytes());
        bad[0x74..0x78].copy_from_slice(&size.to_le_bytes());
        assert!(read(&bad).is_err());
    }
    for offset in [0, 0x100] {
        let mut bad = valid.clone();
        bad[offset] = 0;
        assert!(read(&bad).is_err());
    }
    // A minimally sized ACI0 section still holds a complete identity.
    let mut minimal = valid[..0x118].to_vec();
    minimal[0x74..0x78].copy_from_slice(&0x18u32.to_le_bytes());
    assert_eq!(read(&minimal).unwrap(), 42);
}

const APP: u64 = 0x0400_4860_24DE_C000;

fn code(main: &[u8]) -> Vec<Node> {
    vec![
        Node::File("main", main.to_vec(), true),
        Node::File("main.npdm", npdm(APP), false),
        Node::File(".hidden", b"x".to_vec(), false),
        Node::File("sdk", vec![7u8; 3000], true),
    ]
}

fn romfs(tag: u8) -> Vec<Node> {
    vec![
        Node::File("b.bin", vec![tag; 70_000], true),
        Node::Dir(
            "Data",
            vec![
                Node::File("z.txt", b"zz".to_vec(), false),
                Node::File("empty", Vec::new(), false),
            ],
        ),
        Node::File("A.bin", vec![tag ^ 0x55; 33], false),
    ]
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::var_os("NXB_TEST_TMP")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(format!("nxb-nup-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn req(nup: &Path, work: &Path) -> NupRequest {
    NupRequest {
        nup: nup.to_path_buf(),
        version: None,
        work: work.to_path_buf(),
        threads: 2,
        mem_limit: 64 << 20,
        discard_images: false,
        digests: None,
        skip_images: false,
        progress: None,
    }
}

/// (paths in index order, (path, offset, size) of files) of a DMROMFS1 image.
fn read_image(img: &[u8]) -> Vec<(String, bool, u64, u64)> {
    let u64at = |o: usize| u64::from_le_bytes(img[o..o + 8].try_into().unwrap());
    assert_eq!(&img[..8], b"DMROMFS1");
    let (idx, n) = (u64at(8) as usize, u64at(24) as usize);
    let names = idx + n * 24;
    (0..n)
        .map(|i| {
            let e = idx + i * 24;
            let no = u32::from_le_bytes(img[e + 16..e + 20].try_into().unwrap()) as usize;
            let nl = u16::from_le_bytes(img[e + 20..e + 22].try_into().unwrap()) as usize;
            (
                String::from_utf8_lossy(&img[names + no..names + no + nl]).into_owned(),
                img[e + 22] == 0,
                u64at(e),
                u64at(e + 8),
            )
        })
        .collect()
}

#[test]
fn base_update_and_dlc() {
    let d = tmp("full");
    let pkgs = [
        Pkg {
            title: APP,
            kind: 0,
            version: 0,
            flags: 0,
            exefs: Some(code(b"base main")),
            romfs: Some(romfs(1)),
            meta: vec![(2, vec![9u8; 64]), (3, b"JPEG".to_vec())],
        },
        Pkg {
            title: APP + 0x800,
            kind: 1,
            version: 65536,
            flags: 0,
            exefs: Some(code(b"update main")),
            romfs: Some(romfs(2)),
            meta: vec![],
        },
        Pkg {
            title: APP + 0x1001,
            kind: 2,
            version: 0,
            flags: 0,
            exefs: None,
            romfs: Some(romfs(3)),
            meta: vec![],
        },
    ];
    let nup = d.join("g.nup");
    std::fs::write(&nup, build(&pkgs, false)).unwrap();
    // NXB_NUP_DUMP=<file>: keep the container, for a run of the engine's packer as the oracle
    if let Some(p) = std::env::var_os("NXB_NUP_DUMP") {
        std::fs::copy(&nup, p).unwrap();
    }
    let r = extract_nup(&req(&nup, &d)).unwrap();
    assert_eq!(r.len(), 2);
    let out = d.join("extract/v65536");
    assert_eq!(
        std::fs::read(out.join("ExeFS/main")).unwrap(),
        b"update main"
    );
    assert_eq!(
        std::fs::read(out.join("ExeFS/sdk")).unwrap(),
        vec![7u8; 3000]
    );
    assert!(!out.join("ExeFS/.hidden").exists());
    // the update carries no control data: none written (no fallback while a base game is there)
    assert!(!out.join("control.nacp").exists());
    let img = std::fs::read(out.join("RomFS.img")).unwrap();
    let ents = read_image(&img);
    let names: Vec<&str> = ents.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(names, ["", "A.bin", "Data", "b.bin", "empty", "z.txt"]);
    let get = |n: &str| {
        ents.iter()
            .find(|e| e.0 == n)
            .map(|e| img[e.2 as usize..(e.2 + e.3) as usize].to_vec())
            .unwrap()
    };
    assert_eq!(get("b.bin"), vec![2u8; 70_000]);
    assert_eq!(get("A.bin"), vec![2u8 ^ 0x55; 33]);
    assert_eq!(get("z.txt"), b"zz");
    let aoc = std::fs::read(d.join("aoc/1.img")).unwrap();
    assert_eq!(read_image(&aoc).len(), 6);
    // the base version: its own control data
    let mut rb = req(&nup, &d);
    rb.version = Some(0);
    extract_nup(&rb).unwrap();
    assert_eq!(
        std::fs::read(d.join("extract/v0/control.nacp")).unwrap(),
        vec![9u8; 64]
    );
    assert_eq!(
        std::fs::read(d.join("extract/v0/icon.jpg")).unwrap(),
        b"JPEG"
    );
    // digests only: no image file, the same digest
    let d2 = tmp("discard");
    let mut rd = req(&nup, &d2);
    rd.discard_images = true;
    let r2 = extract_nup(&rd).unwrap();
    assert!(!d2.join("extract/v65536/RomFS.img").exists());
    assert_eq!(r2[0].digest, r[0].digest);
    assert_eq!(r2[0].digest.file.sha256, nxb_model::sha256(&img));
}

#[test]
fn standalone_update_is_the_whole_game() {
    let d = tmp("standalone");
    let pkgs = [Pkg {
        title: APP + 0x800,
        kind: 1,
        version: 131072,
        flags: 1,
        exefs: Some(code(b"m")),
        romfs: Some(romfs(4)),
        meta: vec![(
            10,
            [
                vec![5u8],
                b"en-US".to_vec(),
                3u32.to_le_bytes().to_vec(),
                b"ICO".to_vec(),
            ]
            .concat(),
        )],
    }];
    let nup = d.join("s.nup");
    std::fs::write(&nup, build(&pkgs, false)).unwrap();
    extract_nup(&req(&nup, &d)).unwrap();
    assert_eq!(
        std::fs::read(d.join("extract/v131072/icon.jpg")).unwrap(),
        b"ICO"
    );
    let mut r0 = req(&nup, &d);
    r0.version = Some(0);
    assert!(
        extract_nup(&r0).is_err(),
        "no version 0 without a base game"
    );
    // an update that is not standalone needs a base game
    let pkgs = [Pkg {
        flags: 0,
        ..Pkg {
            title: APP + 0x800,
            kind: 1,
            version: 1,
            flags: 0,
            exefs: Some(code(b"m")),
            romfs: Some(romfs(4)),
            meta: vec![],
        }
    }];
    std::fs::write(&nup, build(&pkgs, false)).unwrap();
    let e = extract_nup(&req(&nup, &d))
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(e.contains("no base game"), "{e}");
}

#[test]
fn damaged_and_hostile_containers_are_refused() {
    let d = tmp("bad");
    let good = [Pkg {
        title: APP,
        kind: 0,
        version: 0,
        flags: 0,
        exefs: Some(code(b"m")),
        romfs: Some(romfs(1)),
        meta: vec![],
    }];
    let nup = d.join("b.nup");
    std::fs::write(&nup, build(&good, true)).unwrap();
    assert!(
        extract_nup(&req(&nup, &d))
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default()
            .contains("SHA-256")
    );
    // cut short
    let full = build(&good, false);
    std::fs::write(&nup, &full[..full.len() - 1]).unwrap();
    assert!(extract_nup(&req(&nup, &d)).is_err());
    // a file whose content does not match its SHA-256
    let mut flipped = full.clone();
    let last = flipped.len() - 1;
    flipped[last - 10] ^= 0xff;
    std::fs::write(&nup, &flipped).unwrap();
    assert!(extract_nup(&req(&nup, &d)).is_err());
    // an ExeFS name that would leave the folder
    let mut ex = code(b"m");
    ex.push(Node::File("../evil", b"x".to_vec(), false));
    let hostile = [Pkg {
        exefs: Some(ex),
        ..Pkg {
            title: APP,
            kind: 0,
            version: 0,
            flags: 0,
            exefs: None,
            romfs: Some(romfs(1)),
            meta: vec![],
        }
    }];
    std::fs::write(&nup, build(&hostile, false)).unwrap();
    let e = extract_nup(&req(&nup, &d))
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(e.contains("invalid ExeFS name"), "{e}");
    assert!(!d.join("extract/evil").exists());
    // not a NUP
    std::fs::write(&nup, b"NSP0 something else").unwrap();
    assert!(extract_nup(&req(&nup, &d)).is_err());
}

#[test]
fn selected_dlc_checks_identity_uniqueness_and_file_hashes() {
    use nxb_extract::nup::unpack_dlc;
    let d = tmp("selected-dlc");
    let path = d.join("dlc.nup");
    let out = d.join("out");
    let title = APP + 0x1001;
    let package = |version| Pkg {
        title,
        kind: 2,
        version,
        flags: 0,
        exefs: None,
        romfs: Some(vec![Node::File("extra", vec![version as u8; 16], false)]),
        meta: vec![],
    };
    std::fs::write(&path, build(&[package(1), package(2)], false)).unwrap();
    unpack_dlc(&path, title, 2, &out, 64 << 20).unwrap();
    assert_eq!(std::fs::read(out.join("extra")).unwrap(), vec![2; 16]);
    for (id, version) in [(title + 1, 2), (title, 3)] {
        let e = unpack_dlc(&path, id, version, &out, 64 << 20).unwrap_err();
        assert!(e.to_string().contains("missing"), "{e}");
    }
    std::fs::write(&path, build(&[package(2), package(2)], false)).unwrap();
    let e = unpack_dlc(&path, title, 2, &out, 64 << 20).unwrap_err();
    assert!(e.to_string().contains("duplicated"), "{e}");
    let update = Pkg {
        kind: 3,
        ..package(3)
    };
    std::fs::write(&path, build(&[package(2), update], false)).unwrap();
    let e = unpack_dlc(&path, title, 2, &out, 64 << 20).unwrap_err();
    assert!(e.to_string().contains("addon_update"), "{e}");
    let absent = Pkg {
        romfs: None,
        ..package(2)
    };
    std::fs::write(&path, build(&[absent], false)).unwrap();
    let e = unpack_dlc(&path, title, 2, &out, 64 << 20).unwrap_err();
    assert!(e.to_string().contains("no RomFS"), "{e}");
    let mut damaged = build(&[package(2)], false);
    *damaged.last_mut().unwrap() ^= 1;
    std::fs::write(&path, damaged).unwrap();
    let e = unpack_dlc(&path, title, 2, &out, 64 << 20).unwrap_err();
    assert!(e.to_string().contains("SHA-256"), "{e}");
    std::fs::remove_dir_all(d).unwrap();
}

fn loose_request(path: &Path, work: &Path) -> nxb_extract::nup::UnpackRequest {
    nxb_extract::nup::UnpackRequest {
        nup: path.to_path_buf(),
        version: None,
        out: work.join("loose"),
        romfs: work.join("loose/romfs"),
        aoc: work.join("loose/aoc"),
        threads: 2,
        mem_limit: 64 << 20,
        verify: true,
        split: None,
        progress: None,
    }
}

fn addon_fixture(version: u32) -> Pkg {
    Pkg {
        title: APP + 0x1001,
        kind: 2,
        version,
        flags: 0,
        exefs: None,
        romfs: Some(vec![
            Node::File("value", vec![version as u8], false),
            Node::File(if version == 1 { "old" } else { "empty" }, vec![], false),
        ]),
        meta: vec![],
    }
}

fn addon_base() -> Pkg {
    Pkg {
        title: APP,
        kind: 0,
        version: 0,
        flags: 0,
        exefs: Some(code(b"main")),
        romfs: Some(romfs(1)),
        meta: vec![],
    }
}

#[test]
fn complete_addon_versions_replace_the_tree_in_either_record_order() {
    use nxb_extract::nup::{Nup, scan, unpack_dlc, unpack_nup};
    let d = tmp("addon-versions");
    let mut reference = None;
    for versions in [[1, 2], [2, 1]] {
        let work = d.join(versions[0].to_string());
        std::fs::create_dir_all(&work).unwrap();
        let path = work.join("game.nup");
        std::fs::write(
            &path,
            build(
                &[
                    addon_base(),
                    addon_fixture(versions[0]),
                    addon_fixture(versions[1]),
                ],
                false,
            ),
        )
        .unwrap();
        let n = Nup::open(&path).unwrap();
        let selected = scan(&n).unwrap().dlc[&(APP + 0x1001)];
        assert_eq!(n.packages[selected].version, 2);
        extract_nup(&req(&path, &work)).unwrap();
        let image = std::fs::read(work.join("aoc/1.img")).unwrap();
        let entries = read_image(&image);
        assert!(!entries.iter().any(|e| e.0 == "old"));
        assert!(entries.iter().any(|e| e.0 == "empty" && e.3 == 0));
        let value = entries.iter().find(|e| e.0 == "value").unwrap();
        assert_eq!(&image[value.2 as usize..(value.2 + value.3) as usize], &[2]);
        if let Some(previous) = &reference {
            assert_eq!(&image, previous);
        }
        reference = Some(image);
        unpack_nup(&loose_request(&path, &work)).unwrap();
        let out = work.join("loose/aoc/1");
        assert_eq!(std::fs::read(out.join("value")).unwrap(), [2]);
        assert_eq!(std::fs::metadata(out.join("empty")).unwrap().len(), 0);
        assert!(!out.join("old").exists());
        // Exact selection can still retrieve the older complete tree, replacing the output.
        unpack_dlc(&path, APP + 0x1001, 1, &out, 64 << 20).unwrap();
        assert_eq!(std::fs::read(out.join("value")).unwrap(), [1]);
        assert!(out.join("old").is_file());
        assert!(!out.join("empty").exists());
        unpack_dlc(&path, APP + 0x1001, 2, &out, 64 << 20).unwrap();
        assert!(!out.join("old").exists());
        assert_eq!(std::fs::metadata(out.join("empty")).unwrap().len(), 0);
    }
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn invalid_addon_records_are_rejected_by_each_consumer() {
    use nxb_extract::nup::{Nup, scan, unpack_dlc, unpack_nup};
    let d = tmp("addon-invalid");
    for case in [
        "duplicate",
        "invalid-id",
        "missing-romfs",
        "wrong-family",
        "reserved-kind",
    ] {
        let mut addon = addon_fixture(1);
        match case {
            "invalid-id" => addon.title = APP + 0x1000,
            "missing-romfs" => addon.romfs = None,
            "wrong-family" => addon.title += 0x2000,
            "reserved-kind" => addon.kind = 3,
            _ => {}
        }
        let mut packages = vec![addon_base(), addon];
        if case == "duplicate" || case == "reserved-kind" {
            packages.push(addon_fixture(1));
        }
        let path = d.join(format!("{case}.nup"));
        std::fs::write(&path, build(&packages, false)).unwrap();
        assert!(
            scan(&Nup::open(&path).unwrap()).is_err(),
            "scan accepted {case}"
        );
        assert!(
            extract_nup(&req(&path, &d)).is_err(),
            "image extraction accepted {case}"
        );
        assert!(
            unpack_nup(&loose_request(&path, &d)).is_err(),
            "loose extraction accepted {case}"
        );
        assert!(
            unpack_dlc(&path, APP + 0x1001, 1, &d.join("selected"), 64 << 20).is_err(),
            "exact selection accepted {case}"
        );
    }
    std::fs::remove_dir_all(d).unwrap();
}
