//! Generic adapter tests with the extract crate's authored package fixtures.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use crate::{extract_fixture as common, nup_fixture as nup};
fn compressed_program(nca: &[u8], plain: &[common::Plain]) -> Vec<u8> {
    let mut out = nca[..0x4000].to_vec();
    out.extend_from_slice(b"NCZSECTN");
    out.extend_from_slice(&(plain.len() as u64).to_le_bytes());
    let mut body = Vec::new();
    for (offset, bytes, fs) in plain {
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        out.extend_from_slice(&3u64.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&common::CTR_KEY);
        let mut upper = fs[0x140..0x148].to_vec();
        upper.reverse();
        out.extend_from_slice(&upper);
        out.extend_from_slice(&[0; 8]);
        body.extend_from_slice(bytes);
    }
    out.extend_from_slice(&[0x28, 0xb5, 0x2f, 0xfd, 0, 0x58]);
    let count = body.chunks(1 << 17).count();
    for (index, chunk) in body.chunks(1 << 17).enumerate() {
        out.extend_from_slice(
            &(((chunk.len() as u32) << 3) | u32::from(index + 1 == count)).to_le_bytes()[..3],
        );
        out.extend_from_slice(chunk);
    }
    out
}

#[test]
fn packages_ncz_separate_updates_and_dlc_share_the_extractor() {
    const APP: u64 = 0x0100000000000000;
    let dir = common::tmp("generic-inputs");
    let keys = common::write_keys(&dir);
    let base = common::title(0x80, APP, 0, 1, true);
    let mut patch = common::title(0x81, APP, 65536, 2, true);
    // A code-only update keeps the base RomFS. Data updates require a BKTR section.
    patch.files[0].1 = common::nca(
        0,
        APP,
        vec![(
            0,
            common::pfs0_section(&[
                ("main".into(), b"NSO0patched".to_vec()),
                ("main.npdm".into(), common::npdm(APP)),
            ]),
        )],
    )
    .0;
    let plain = dir.join("base.nsp");
    std::fs::write(&plain, common::pfs0(&base.files)).unwrap();
    let mut compressed = base.files.clone();
    let program = format!("{}.nca", common::hex(&base.program_id));
    let entry = compressed
        .iter_mut()
        .find(|(name, _)| name == &program)
        .unwrap();
    entry.0 = entry.0.replace(".nca", ".ncz");
    entry.1 = compressed_program(&entry.1, &base.plain);
    let nsz = dir.join("base.nsz");
    std::fs::write(&nsz, common::pfs0(&compressed)).unwrap();
    let xci = dir.join("base.xci");
    std::fs::write(&xci, common::xci(&[("secure", base.files.clone())])).unwrap();
    let xcz = dir.join("base.xcz");
    std::fs::write(&xcz, common::xci(&[("secure", compressed)])).unwrap();
    let updates = dir.join("updates");
    std::fs::create_dir(&updates).unwrap();
    std::fs::write(
        updates.join("misleading [v0].nsp"),
        common::pfs0(&patch.files),
    )
    .unwrap();
    let dlc = dir.join("dlc.nsp");
    std::fs::write(&dlc, dlc_package(APP + 0x1001, 3, b"dlc", false)).unwrap();
    let mut args = Args {
        nsp_dir: plain,
        keys: Some(keys),
        threads: 2,
        ..Args::default()
    };
    let progress = Progress::new(crate::events::Mode::Off, Box::new(std::io::sink()));
    let original = extract(&args, &progress).unwrap();
    for path in [nsz, xci, xcz] {
        args.nsp_dir = path;
        let game = extract(&args, &progress).unwrap();
        assert_eq!(original.files, game.files);
        assert_eq!(original.title_id, game.title_id);
    }
    args.update_inputs.push(updates);
    args.aoc.push(dlc);
    let updated = extract(&args, &progress).unwrap();
    assert_eq!(updated.version, 65536);
    assert_eq!(
        updated.modules,
        vec![("main".into(), b"NSO0patched".to_vec())]
    );
    assert!(
        updated
            .files
            .iter()
            .any(|(n, bytes)| n == "romfs/small.txt" && bytes == b"tag 1")
    );
    assert!(
        updated
            .files
            .iter()
            .any(|(n, bytes)| n == "aoc/1/extra" && bytes == b"dlc")
    );
    assert_eq!(updated.aoc[0].index, 1);
    assert_eq!(updated.aoc[0].version, 3);
    common::cleanup(&dir);
}

fn dlc_package(title: u64, version: u32, bytes: &[u8], compressed: bool) -> Vec<u8> {
    common::pfs0(&dlc_files(title, version, bytes, compressed))
}

fn dlc_files(title: u64, version: u32, bytes: &[u8], compressed: bool) -> common::Files {
    let content_id = [7; 16];
    let (data, plain) = common::nca(
        5,
        title,
        vec![(0, common::romfs_section(&[("extra", bytes.to_vec())]))],
    );
    let (extension, data) = if compressed {
        ("ncz", compressed_program(&data, &plain))
    } else {
        ("nca", data)
    };
    let (meta, _) = common::nca(
        1,
        title,
        vec![(
            0,
            common::pfs0_section(&[(
                "dlc.cnmt".into(),
                common::cnmt(0x82, title, version, &[content_id]),
            )]),
        )],
    );
    vec![
        (format!("{}.{extension}", common::hex(&content_id)), data),
        (format!("{}.cnmt.nca", common::hex(&[8; 16])), meta),
    ]
}

fn nup_dlc(title: u64, version: u32, bytes: &[u8]) -> nup::Pkg {
    nup::Pkg {
        title,
        kind: 2,
        version,
        flags: 0,
        exefs: None,
        romfs: Some(vec![nup::Node::File("extra", bytes.to_vec(), true)]),
        meta: vec![],
    }
}

fn check_dlc(game: &Extracted, app: u64, version: u32, bytes: &[u8]) {
    assert_eq!(game.title_id, app);
    assert_eq!(game.aoc.len(), 1);
    assert_eq!(game.aoc[0].title_id, app + 0x1001);
    assert_eq!(game.aoc[0].index, 1);
    assert_eq!(game.aoc[0].version, version);
    assert_eq!(game.aoc[0].path, "aoc/1");
    let files: Vec<_> = game
        .files
        .iter()
        .filter(|(n, _)| n.starts_with("aoc/"))
        .collect();
    assert_eq!(files, vec![&("aoc/1/extra".into(), bytes.to_vec())]);
}

#[test]
fn nup_base_selects_newest_embedded_or_external_dlc() {
    const APP: u64 = 0x0100000000000000;
    let dir = common::tmp("generic-nup-dlc");
    let base = nup::Pkg {
        title: APP,
        kind: 0,
        version: 0,
        flags: 0,
        exefs: Some(vec![
            nup::Node::File("main", b"NSO0base".to_vec(), true),
            nup::Node::File("main.npdm", common::npdm(APP), false),
        ]),
        romfs: Some(vec![nup::Node::File("base", b"base".to_vec(), false)]),
        meta: vec![],
    };
    let mut embedded = nup_dlc(APP + 0x1001, 2, b"embedded");
    embedded
        .romfs
        .as_mut()
        .unwrap()
        .push(nup::Node::File("removed", b"old file".to_vec(), false));
    let path = dir.join("base.nup");
    std::fs::write(&path, nup::build(&[base, embedded], false)).unwrap();
    let mut args = Args {
        nsp_dir: path,
        keys: Some(common::write_keys(&dir)),
        threads: 2,
        ..Args::default()
    };
    let progress = Progress::new(crate::events::Mode::Off, Box::new(std::io::sink()));
    let original = extract(&args, &progress).unwrap();
    assert_eq!(
        original.modules,
        vec![("main".into(), b"NSO0base".to_vec())]
    );
    assert_eq!(original.aoc[0].version, 2);
    assert!(original.files.iter().any(|(n, _)| n == "aoc/1/removed"));
    let old = dir.join("old.nsp");
    std::fs::write(&old, dlc_package(APP + 0x1001, 1, b"old", false)).unwrap();
    args.aoc.push(old);
    assert_eq!(extract(&args, &progress).unwrap().files, original.files);
    for (version, extension, compressed) in [
        (3, "nsp", false),
        (4, "nsz", true),
        (5, "xci", false),
        (6, "xcz", true),
    ] {
        let path = dir.join(format!("v{version}.{extension}"));
        let bytes = format!("external {version}").into_bytes();
        let files = dlc_files(APP + 0x1001, version, &bytes, compressed);
        let container = if extension.starts_with('x') {
            common::xci(&[("secure", files)])
        } else {
            common::pfs0(&files)
        };
        std::fs::write(&path, container).unwrap();
        args.aoc.push(path);
        let game = extract(&args, &progress).unwrap();
        check_dlc(&game, APP, version, &bytes);
        assert!(
            game.files
                .iter()
                .any(|(n, b)| n == "romfs/base" && b == b"base")
        );
    }
    let dlc = dir.join("dlc.nup");
    std::fs::write(
        &dlc,
        nup::build(&[nup_dlc(APP + 0x1001, 7, b"NUP DLC")], false),
    )
    .unwrap();
    args.aoc.push(dlc.clone());
    check_dlc(&extract(&args, &progress).unwrap(), APP, 7, b"NUP DLC");
    args.aoc.reverse();
    check_dlc(&extract(&args, &progress).unwrap(), APP, 7, b"NUP DLC");
    // A DLC-only NUP also works with an ordinary package base.
    let package = common::title(0x80, APP, 0, 1, false);
    let mut compressed = package.files.clone();
    let program = &mut compressed[0];
    program.0 = program.0.replace(".nca", ".ncz");
    program.1 = compressed_program(&program.1, &package.plain);
    args.aoc = vec![dlc];
    for (extension, files) in [
        ("nsp", &package.files),
        ("nsz", &compressed),
        ("xci", &package.files),
        ("xcz", &compressed),
    ] {
        args.nsp_dir = dir.join(format!("base.{extension}"));
        let container = if extension.starts_with('x') {
            common::xci(&[("secure", files.clone())])
        } else {
            common::pfs0(files)
        };
        std::fs::write(&args.nsp_dir, container).unwrap();
        check_dlc(&extract(&args, &progress).unwrap(), APP, 7, b"NUP DLC");
    }
    // Content from another application is rejected for either base format.
    for extension in ["nsp", "nup"] {
        let wrong = dir.join(format!("wrong.{extension}"));
        let title = APP + 0x2000 + 0x1001;
        let bytes = if extension == "nsp" {
            dlc_package(title, 6, b"wrong", false)
        } else {
            nup::build(&[nup_dlc(title, 6, b"wrong")], false)
        };
        std::fs::write(&wrong, bytes).unwrap();
        args.aoc = vec![wrong];
        for base in ["base.nsp", "base.nup"] {
            args.nsp_dir = dir.join(base);
            assert!(
                extract(&args, &progress).is_err(),
                "{base} accepted wrong-family {extension}"
            );
        }
    }
    common::cleanup(&dir);
}

fn nup_program(app: u64, version: u32, main: &[u8]) -> nup::Pkg {
    nup::Pkg {
        title: app + if version == 0 { 0 } else { 0x800 },
        kind: u8::from(version != 0),
        version,
        flags: u8::from(version != 0),
        exefs: Some(vec![
            nup::Node::File("main", main.to_vec(), true),
            nup::Node::File("main.npdm", common::npdm(app), false),
        ]),
        romfs: Some(vec![nup::Node::File("data", main.to_vec(), true)]),
        meta: vec![],
    }
}

#[test]
fn standalone_nup_updates_replace_either_base_and_merge_dlc() {
    const APP: u64 = 0x0100000000000000;
    let dir = common::tmp("generic-nup-update");
    let keys = common::write_keys(&dir);
    let mut package = common::title(0x80, APP, 0, 1, false).files;
    package.extend(dlc_files(APP + 0x1001, 8, b"base DLC", false));
    std::fs::write(dir.join("base.nsp"), common::pfs0(&package)).unwrap();
    std::fs::write(
        dir.join("base.nup"),
        nup::build(
            &[
                nup_program(APP, 0, b"NSO0base"),
                nup_dlc(APP + 0x1001, 8, b"base DLC"),
            ],
            false,
        ),
    )
    .unwrap();
    let updates = dir.join("updates");
    std::fs::create_dir(&updates).unwrap();
    let latest = updates.join("misleading [v0].nup");
    std::fs::write(
        &latest,
        nup::build(
            &[
                nup_program(APP, 131072, b"NSO0latest"),
                nup_dlc(APP + 0x1001, 4, b"older DLC"),
                nup_dlc(APP + 0x1002, 3, b"latest DLC"),
            ],
            false,
        ),
    )
    .unwrap();
    let older = updates.join("older.nup");
    std::fs::write(
        &older,
        nup::build(
            &[
                nup_program(APP, 65536, b"NSO0older"),
                nup_dlc(APP + 0x1002, 1, b"old DLC"),
                nup_dlc(APP + 0x1003, 2, b"retained DLC"),
            ],
            false,
        ),
    )
    .unwrap();
    let progress = Progress::new(crate::events::Mode::Off, Box::new(std::io::sink()));
    for base in ["base.nsp", "base.nup"] {
        let mut args = Args {
            nsp_dir: dir.join(base),
            keys: Some(keys.clone()),
            threads: 2,
            update_inputs: vec![updates.clone()],
            ..Args::default()
        };
        for inputs in [
            vec![updates.clone()],
            vec![latest.clone(), older.clone()],
            vec![older.clone(), latest.clone()],
        ] {
            args.update_inputs = inputs;
            let game = extract(&args, &progress).unwrap();
            assert_eq!(game.title_id, APP);
            assert_eq!(game.version, 131072);
            assert_eq!(game.modules, vec![("main".into(), b"NSO0latest".to_vec())]);
            assert!(
                game.files
                    .iter()
                    .any(|(n, b)| n == "romfs/data" && b == b"NSO0latest")
            );
            assert_eq!(
                game.aoc
                    .iter()
                    .map(|a| (a.index, a.version))
                    .collect::<Vec<_>>(),
                [(1, 8), (2, 3), (3, 2)]
            );
            for (index, bytes) in [
                (1, b"base DLC".as_slice()),
                (2, b"latest DLC"),
                (3, b"retained DLC"),
            ] {
                assert!(
                    game.files
                        .iter()
                        .any(|(n, b)| n == &format!("aoc/{index}/extra") && b == bytes)
                );
            }
        }
        // Incomplete, non-standalone and unrelated updates cannot replace either base.
        for invalid in 0..4 {
            let mut patch = nup_program(APP, 196608, b"NSO0bad");
            match invalid {
                0 => patch.flags = 0,
                1 => patch.exefs = None,
                2 => patch.romfs = None,
                _ => patch = nup_program(APP + 0x2000, 196608, b"NSO0other"),
            }
            let path = dir.join("invalid.nup");
            std::fs::write(&path, nup::build(&[patch], false)).unwrap();
            args.update_inputs = vec![path];
            assert!(
                extract(&args, &progress).is_err(),
                "{base} accepted invalid update {invalid}"
            );
        }
    }
    common::cleanup(&dir);
}

#[test]
fn nup_addon_updates_are_not_silently_ignored() {
    const APP: u64 = 0x0100000000000000;
    let dir = common::tmp("generic-nup-addon-update");
    let mixed = || {
        vec![
            nup_dlc(APP + 0x1001, 1, b"old"),
            nup::Pkg {
                kind: 3,
                ..nup_dlc(APP + 0x1001, 2, b"new")
            },
        ]
    };
    let mut packages = vec![nup_program(APP, 0, b"NSO0base")];
    packages.extend(mixed());
    let primary = dir.join("primary.nup");
    std::fs::write(&primary, nup::build(&packages, false)).unwrap();
    let progress = Progress::new(crate::events::Mode::Off, Box::new(std::io::sink()));
    let mut args = Args {
        nsp_dir: primary,
        threads: 2,
        ..Args::default()
    };
    let e = extract(&args, &progress).err().unwrap();
    assert!(e.contains("addon_update"), "{e}");
    let dlc = dir.join("dlc.nup");
    std::fs::write(&dlc, nup::build(&mixed(), false)).unwrap();
    args.nsp_dir = dir.join("base.nup");
    std::fs::write(
        &args.nsp_dir,
        nup::build(&[nup_program(APP, 0, b"NSO0base")], false),
    )
    .unwrap();
    args.aoc.push(dlc);
    let e = extract(&args, &progress).err().unwrap();
    assert!(e.contains("addon_update"), "{e}");
    common::cleanup(&dir);
}

#[path = "extract_tests/encrypted_updates.rs"]
mod encrypted_updates;

#[path = "extract_tests/empty_romfs.rs"]
mod empty_romfs;
