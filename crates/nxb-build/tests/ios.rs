//! Authored encrypted NSP -> native rewrite + iOS shader -> unsigned IPA regression.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "../../nxb-extract/tests/common/mod.rs"]
mod common;
use std::collections::BTreeMap;
fn u32b(b: &mut [u8], at: usize, n: u32) {
    b[at..at + 4].copy_from_slice(&n.to_le_bytes());
}
fn u64b(b: &mut [u8], at: usize, n: u64) {
    b[at..at + 8].copy_from_slice(&n.to_le_bytes());
}
fn nso() -> Vec<u8> {
    let mut b = vec![0; 0x5100];
    b[..4].copy_from_slice(b"NSO0");
    b[0x40..0x50].fill(1);
    for (i, (offset, size)) in [(0, 0x1000), (0x1000, 0x3000), (0x4000, 0x1000)]
        .into_iter()
        .enumerate()
    {
        u32b(&mut b, 0x10 + i * 16, 0x100 + offset);
        u32b(&mut b, 0x14 + i * 16, offset);
        u32b(&mut b, 0x18 + i * 16, size);
        u32b(&mut b, 0x60 + i * 4, size);
    }
    u32b(&mut b, 0x3c, 0x1000);
    for at in (0x100..0x1100).step_by(4) {
        u32b(&mut b, at, 0xd503201f);
    }
    for (at, n) in [
        (0x100, 0x14000010),
        (0x104, 0x1000),
        (0x140, 0xd4000541),
        (0x144, 0xd65f03c0),
        (0x1104, 0x40),
        (0x1110, 0x180),
        (0x1304, 1),
    ] {
        u32b(&mut b, at, n);
    }
    b[0x1100..0x1104].copy_from_slice(b"MOD0");
    b[0x1280..0x1284].copy_from_slice(&[1, 3, 3, 3]);
    for (i, (tag, value)) in [(4, 0x1200), (5, 0x1300), (6, 0x1240), (0, 0)]
        .into_iter()
        .enumerate()
    {
        u64b(&mut b, 0x1140 + i * 16, tag);
        u64b(&mut b, 0x1148 + i * 16, value);
    }
    b
}
fn shader() -> Vec<u8> {
    let mut b = vec![0; 0x9c0 + 24];
    b[..4].copy_from_slice(b"BNSH");
    b[0x60..0x64].copy_from_slice(b"grsc");
    for (at, n) in [
        (0x7c, 1),
        (0x138, 24),
        (0x13c, 0x880),
        (0x140, 0x98761234),
        (0x160, 0x880),
        (0x840, 24),
        (0x844, 24),
        (0x888, 1),
        (0x88c, 1),
        (0x890, 1),
    ] {
        u32b(&mut b, at, n);
    }
    for (at, n) in [
        (0x80, 0x90),
        (0xa0, 0xd0),
        (0xd0 + 8 + 8 * 5, 0x120),
        (0x128, 0x140),
        (0x130, 0x9c0),
        (0x9c8, 0xe30000000007000f),
    ] {
        u64b(&mut b, at, n);
    }
    b
}
fn entries(bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut at = 0;
    let u16at = |at| u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap()) as usize;
    while bytes.get(at..at + 4) == Some(b"PK\x03\x04") {
        assert_eq!(u16at(at + 8), 0);
        let names = u16at(at + 26);
        let extras = u16at(at + 28);
        let name = String::from_utf8(bytes[at + 30..at + 30 + names].to_vec()).unwrap();
        let extra = at + 30 + names;
        assert_eq!(u16at(extra), 1);
        let size = u64::from_le_bytes(bytes[extra + 4..extra + 12].try_into().unwrap()) as usize;
        at = extra + extras;
        out.insert(name, bytes[at..at + size].to_vec());
        at += size;
    }
    out
}
#[test]
fn native_unsigned_single_game_ipa() {
    let dir = common::tmp("ios-native-e2e");
    let keys = common::write_keys(&dir);
    let title = 0x0100000000000000;
    let mut npdm = common::npdm(title);
    npdm[0xe] = 44;
    u32b(&mut npdm, 0x14, 0x10000);
    u32b(&mut npdm, 0x1c, 0x100000);
    let (program, _) = common::nca(
        0,
        title,
        vec![
            (
                0,
                common::pfs0_section(&[("main".into(), nso()), ("main.npdm".into(), npdm)]),
            ),
            (
                1,
                common::romfs_section(&[
                    ("message.txt", b"authored resource".to_vec()),
                    ("test.bnsh", shader()),
                ]),
            ),
        ],
    );
    let id = [1; 16];
    let mut cnmt = common::cnmt(0x80, title, 0, &[id]);
    cnmt[0x30 + 0x36] = 1; // Program content type
    let (meta, _) = common::nca(
        1,
        title,
        vec![(0, common::pfs0_section(&[("fixture.cnmt".into(), cnmt)]))],
    );
    let input = dir.join("fixture.nsp");
    std::fs::write(
        &input,
        common::pfs0(&[
            (format!("{}.nca", common::hex(&id)), program),
            (format!("{}.cnmt.nca", common::hex(&[2; 16])), meta),
        ]),
    )
    .unwrap();
    let host = dir.join("Host.app");
    std::fs::create_dir(&host).unwrap();
    let mut executable = vec![0u8; 56];
    for (at, n) in [
        (0, 0xfeedfacf),
        (4, 0x0100000c),
        (12, 2),
        (16, 1),
        (20, 24),
        (32, 0x32),
        (36, 24),
        (40, 2),
        (44, 26 << 16),
        (48, 26 << 16),
    ] {
        u32b(&mut executable, at, n);
    }
    std::fs::write(host.join("nxkit"), &executable).unwrap();
    std::fs::write(host.join("Info.plist"),b"<plist><dict><key>CFBundleExecutable</key><string>nxkit</string><key>CFBundleIdentifier</key><string>app.nxkit.game</string></dict></plist>").unwrap();
    let output = dir.join("game.ipa");
    let args = vec![
        input.display().to_string(),
        "--auto-analyze".into(),
        "--platform".into(),
        "ios".into(),
        "--host-app".into(),
        host.display().to_string(),
        "--keys".into(),
        keys.display().to_string(),
        "--out".into(),
        output.display().to_string(),
    ];
    if !cfg!(feature = "air-front") {
        // Without a shader compiler the same input must report unsupported, not a generic error.
        assert_eq!(nxb_build::main(&args), 4);
        assert!(!output.exists());
        common::cleanup(&dir);
        return;
    }
    assert_eq!(nxb_build::main(&args), 0);
    let bytes = std::fs::read(&output).unwrap();
    let files = entries(&bytes);
    let root = "Payload/Nxkit.app/";
    assert_eq!(files[&format!("{root}nxkit")], executable);
    assert_eq!(
        files[&format!("{root}romfs/message.txt")],
        b"authored resource"
    );
    assert!(
        !files
            .keys()
            .any(|s| s.contains("CodeSignature") || s.contains("mobileprovision"))
    );
    for extension in [
        "bin",
        "imports.json",
        "exports.json",
        "fixups.tsv",
        "gates.tsv",
    ] {
        assert!(files.contains_key(&format!("{root}modules/main.{extension}")));
    }
    let image = &files[&format!("{root}Frameworks/nxgame.dylib")];
    assert!(image.windows(9).any(|s| s == b"__NX_HEAP"));
    let game = nxb_pyjson::parse(std::str::from_utf8(&files[&format!("{root}game.json")]).unwrap())
        .unwrap();
    assert_eq!(
        game.get("dylib").and_then(nxb_pyjson::Value::as_str),
        Some("Frameworks/nxgame.dylib")
    );
    assert!(game.get("patches").is_none());
    assert!(
        !files
            .keys()
            .any(|p| p.ends_with("patches.toml") || p.ends_with("patches.json"))
    );
    assert!(
        files
            .iter()
            .any(|(p, b)| p.ends_with(".metallib") && b.starts_with(b"MTLB"))
    );
    assert_eq!(nxb_build::main(&args), 1);
    assert_eq!(std::fs::read(&output).unwrap(), bytes); // never overwrite
    // A new output directory is created, and archive bytes remain deterministic.
    let mut again = args.clone();
    *again.last_mut().unwrap() = dir.join("new/output/again.ipa").display().to_string();
    assert_eq!(nxb_build::main(&again), 0);
    assert_eq!(
        std::fs::read(dir.join("new/output/again.ipa")).unwrap(),
        bytes
    );
    // The common build error path preserves the extractor's missing-key exit code.
    let mut missing_keys = args;
    let key = missing_keys.iter().position(|a| a == "--keys").unwrap() + 1;
    missing_keys[key] = dir.join("missing.keys").display().to_string();
    *missing_keys.last_mut().unwrap() = dir.join("missing-key.ipa").display().to_string();
    assert_eq!(nxb_build::main(&missing_keys), 6);
    assert!(!dir.join("missing-key.ipa").exists());
    common::cleanup(&dir);
}

#[test]
fn shared_ios_dry_run_does_not_require_inputs_or_write_output() {
    let dir = common::tmp("ios-dry-run");
    let output = dir.join("game.ipa");
    let args = vec![
        dir.join("missing.nup").display().to_string(),
        "--auto-analyze".into(),
        "--platform=ios".into(),
        "--host-app".into(),
        "Missing.app".into(),
        "--out".into(),
        output.display().to_string(),
        "--dry-run".into(),
    ];
    assert_eq!(nxb_build::main(&args), 0);
    assert!(!output.exists());
    common::cleanup(&dir);
}

#[test]
fn default_ios_dry_run_uses_the_existing_title_module() {
    let dir = common::tmp("ios-title-dry-run");
    let catalog = nxtitles::catalog().unwrap();
    let (title, version) = catalog
        .titles()
        .iter()
        .find_map(|title| {
            let resolved = catalog.resolve(&nxtitle::TitleFacts::new(title.id()));
            resolved.standalone().map(|s| (title.id().0, s.version))
        })
        .unwrap();
    // Authored package headers suffice for the original filename-based dry-run.
    let mut header = vec![0; 0x104];
    header[..4].copy_from_slice(b"PFS0");
    std::fs::write(dir.join(format!("Base [{title:016X}][v0].nsp")), &header).unwrap();
    if version != 0 {
        std::fs::write(
            dir.join(format!("Update [{:016X}][v{version}].nsp", title + 0x800)),
            &header,
        )
        .unwrap();
    }
    let output = dir.join("game.ipa");
    let args = vec![
        dir.display().to_string(),
        "--platform=ios".into(),
        "--host-app".into(),
        "Missing.app".into(),
        "--out".into(),
        output.display().to_string(),
        "--dry-run".into(),
    ];
    assert!(!nxb_build::parse(&args).unwrap().auto_analyze);
    assert_eq!(nxb_build::main(&args), 0);
    assert!(!output.exists());
    common::cleanup(&dir);
}
