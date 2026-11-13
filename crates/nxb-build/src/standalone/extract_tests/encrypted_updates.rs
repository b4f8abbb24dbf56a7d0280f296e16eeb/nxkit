//! Encrypted updates over loose NUP bases use code-only extraction or verified package backing.
use super::*;
#[path = "../../../../nxb-extract/tests/common/sparse.rs"]
mod sparse;

fn nup_base(dir: &Path, app: u64, version: u32) -> PathBuf {
    let path = dir.join(format!("base-{version}.nup"));
    std::fs::write(
        &path,
        nup::build(
            &[
                nup_program(app, version, b"NSO0original"),
                nup_dlc(app + 0x1001, 9, b"NUP-only DLC"),
            ],
            false,
        ),
    )
    .unwrap();
    path
}

#[test]
fn encrypted_code_only_update_preserves_nup_romfs_and_dlc() {
    let dir = common::tmp("generic-nup-code-update");
    let app = sparse::APP;
    let mut patch = common::title(0x81, app, 65536, 2, false);
    patch.files[0].1 = common::nca(
        0,
        app,
        vec![(
            0,
            common::pfs0_section(&[
                ("main".into(), b"NSO0patched".to_vec()),
                ("main.npdm".into(), common::npdm(app)),
            ]),
        )],
    )
    .0;
    let update = dir.join("code.nsp");
    std::fs::write(&update, common::pfs0(&patch.files)).unwrap();
    let mut args = Args {
        nsp_dir: nup_base(&dir, app, 0),
        keys: Some(common::write_keys(&dir)),
        update_inputs: vec![update],
        threads: 2,
        ..Args::default()
    };
    let progress = Progress::new(crate::events::Mode::Off, Box::new(std::io::sink()));
    let game = extract(&args, &progress).unwrap();
    assert_eq!(game.version, 65536);
    assert_eq!(game.modules, vec![("main".into(), b"NSO0patched".to_vec())]);
    assert!(
        game.files
            .iter()
            .any(|(n, b)| n == "romfs/data" && b == b"NSO0original")
    );
    check_dlc(&game, app, 9, b"NUP-only DLC");
    // A cumulative code-only patch retains original base data, not an older update's data.
    std::fs::write(
        &args.nsp_dir,
        nup::build(
            &[
                nup_program(app, 0, b"NSO0original"),
                nup_program(app, 32768, b"NSO0older-update"),
                nup_dlc(app + 0x1001, 9, b"NUP-only DLC"),
            ],
            false,
        ),
    )
    .unwrap();
    let cumulative = extract(&args, &progress).unwrap();
    assert_eq!(cumulative.version, game.version);
    assert_eq!(cumulative.modules, game.modules);
    assert_eq!(cumulative.files, game.files);
    // An update-only NUP does not retain the original base bytes needed by this patch.
    args.nsp_dir = nup_base(&dir, app, 32768);
    assert!(
        extract(&args, &progress).is_err(),
        "accepted update-only NUP without backing"
    );

    common::cleanup(&dir);
}

#[test]
fn encrypted_bktr_update_requires_matching_base_package() {
    let dir = common::tmp("generic-nup-bktr-update");
    let fixture = sparse::fixture();
    let app = sparse::APP;
    let update = dir.join("update.nsp");
    let backing = dir.join("backing.nsp");
    std::fs::write(&update, &fixture.update).unwrap();
    std::fs::write(&backing, &fixture.base).unwrap();
    let original = nup_base(&dir, app, 0);
    let mut args = Args {
        nsp_dir: original.clone(),
        keys: Some(common::write_keys(&dir)),
        update_inputs: vec![update],
        threads: 2,
        ..Args::default()
    };
    let progress = Progress::new(crate::events::Mode::Off, Box::new(std::io::sink()));
    assert!(
        extract(&args, &progress).is_err(),
        "BKTR needs original NCA backing"
    );
    args.base_package = Some(backing.clone());
    let game = extract(&args, &progress).unwrap();
    assert_eq!(game.version, 65536);
    let mut romfs: Vec<_> = game
        .files
        .iter()
        .filter_map(|(name, bytes)| {
            name.strip_prefix("romfs")
                .map(|n| (n.to_string(), bytes.clone()))
        })
        .collect();
    romfs.sort();
    assert_eq!(romfs, fixture.image);
    check_dlc(&game, app, 9, b"NUP-only DLC");
    for (name, title, version) in [("wrong-family", app + 0x2000, 0), ("wrong-version", app, 1)] {
        let path = dir.join(format!("{name}.nsp"));
        std::fs::write(
            &path,
            common::pfs0(&common::title(0x80, title, version, 1, false).files),
        )
        .unwrap();
        args.base_package = Some(path);
        assert!(
            extract(&args, &progress).is_err(),
            "accepted {name} backing"
        );
    }
    args.base_package = Some(dir.join("missing.nsp"));
    assert!(
        extract(&args, &progress).is_err(),
        "accepted missing backing"
    );
    args.base_package = Some(backing.clone());
    args.nsp_dir = nup_base(&dir, app, 131072);
    assert!(extract(&args, &progress).is_err(), "accepted a downgrade");
    args.nsp_dir = backing;
    assert!(
        extract(&args, &progress).is_err(),
        "backing requires a NUP primary"
    );
    args.nsp_dir = original;
    args.update_inputs.clear();
    assert!(
        extract(&args, &progress).is_err(),
        "backing requires an encrypted update"
    );
    common::cleanup(&dir);
}
