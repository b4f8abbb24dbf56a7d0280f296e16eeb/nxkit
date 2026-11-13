//! An absent Program data section is empty RomFS, while an invalid present section is an error.
use super::*;

const APP: u64 = 0x0100_0000_0000_0000;

fn package(version: u32, compressed: bool, data: Option<common::Section>) -> common::Files {
    let mut title = common::title(
        if version == 0 { 0x80 } else { 0x81 },
        APP,
        version,
        1,
        true,
    );
    let mut sections = vec![(
        0,
        common::pfs0_section(&[
            ("main".into(), format!("NSO0v{version}").into_bytes()),
            ("main.npdm".into(), common::npdm(APP)),
        ]),
    )];
    if let Some(data) = data {
        sections.push((1, data));
    }
    let (program, plain) = common::nca(0, APP, sections);
    title.files[0].1 = if compressed {
        compressed_program(&program, &plain)
    } else {
        program
    };
    if compressed {
        title.files[0].0 = title.files[0].0.replace(".nca", ".ncz");
    }
    title.files
}

#[test]
fn absent_program_data_is_empty_for_nsp_nsz_and_code_only_updates() {
    let dir = common::tmp("generic-empty-romfs");
    let keys = common::write_keys(&dir);
    let progress = Progress::new(crate::events::Mode::Off, Box::new(std::io::sink()));
    for (extension, compressed) in [("nsp", false), ("nsz", true)] {
        let base = dir.join(format!("base.{extension}"));
        let update = dir.join(format!("update.{extension}"));
        std::fs::write(&base, common::pfs0(&package(0, compressed, None))).unwrap();
        std::fs::write(&update, common::pfs0(&package(65536, compressed, None))).unwrap();
        let mut args = Args {
            nsp_dir: base,
            keys: Some(keys.clone()),
            threads: 2,
            ..Args::default()
        };
        for version in [0, 65536] {
            if version != 0 {
                args.update_inputs.push(update.clone());
            }
            let game = extract(&args, &progress).unwrap();
            assert_eq!(game.title_id, APP);
            assert_eq!(game.version, version);
            assert_eq!(
                game.modules,
                vec![("main".into(), format!("NSO0v{version}").into_bytes())]
            );
            assert_eq!(game.npdm, common::npdm(APP));
            assert_eq!(game.name, "Test game");
            assert!(game.directories.iter().any(|p| p == "romfs"));
            assert!(!game.files.iter().any(|(p, _)| p.starts_with("romfs/")));
            assert!(game.files.iter().any(|(p, _)| p == "control.nacp"));
            assert!(game.files.iter().any(|(p, _)| p == "icon.jpg"));
            assert!(game.aoc.is_empty());
        }
    }
    common::cleanup(&dir);
}

#[test]
fn invalid_present_program_data_never_falls_back_to_empty() {
    let dir = common::tmp("generic-invalid-romfs");
    let keys = common::write_keys(&dir);
    let progress = Progress::new(crate::events::Mode::Off, Box::new(std::io::sink()));
    for unsupported in [false, true] {
        let mut data = common::romfs_section(&[("data", b"bytes".to_vec())]);
        if unsupported {
            data.fs[3] = 0xff;
        } else {
            data.bytes[0] ^= 1;
        }
        let base = dir.join("base.nsp");
        std::fs::write(&base, common::pfs0(&package(0, false, Some(data)))).unwrap();
        let args = Args {
            nsp_dir: base,
            keys: Some(keys.clone()),
            threads: 2,
            ..Args::default()
        };
        assert!(
            extract(&args, &progress).is_err(),
            "invalid present data became empty"
        );
    }
    common::cleanup(&dir);
}
