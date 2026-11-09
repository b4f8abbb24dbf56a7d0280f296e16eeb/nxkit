//! Tests of the rewrite stage.

use super::{
    Engine, Fail, PatchTable, SOURCES, collected_nros, ensure, file_sha, read_rules, shipped_rules,
};

#[test]
fn shipped_rules_are_found_and_equal_the_shipped_snapshot() {
    let dir = shipped_rules().expect("data/rewrite-rules");
    let table = read_rules(&dir).expect("rule files");
    let snap = std::fs::read_to_string(dir.join("snapshot.tsv")).expect("snapshot");
    let from_snap = PatchTable::parse(&snap).expect("parse");
    assert_eq!(table.to_snapshot(), from_snap.to_snapshot());
    let regenerated = from_snap.to_toml();
    for (name, text) in &regenerated {
        let shipped = std::fs::read_to_string(dir.join(name)).expect("rule file");
        assert_eq!(&shipped, text, "{name} is not what nxb rewrite-rules makes");
    }
    assert_eq!(
        regenerated.len() + 2,
        std::fs::read_dir(&dir).expect("dir").count()
    );
}

#[test]
fn the_engine_files_are_read_as_they_are() {
    let d = std::env::temp_dir().join(format!("nxb-engine-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let tools = d.join("dm/tools");
    std::fs::create_dir_all(&tools).expect("dir");
    for rel in SOURCES {
        std::fs::write(tools.join(rel), format!("# {rel} of any engine")).expect("file");
    }
    let eng = Engine::with_table(&d, PatchTable::default()).expect("the engine is used");
    assert_eq!(eng.sources.len(), SOURCES.len());
    let (sha, rel) = &eng.sources[0];
    assert_eq!(rel, "patch_nso.py");
    assert_eq!(*sha, file_sha(&tools.join("patch_nso.py")).expect("hash"));
    std::fs::remove_file(tools.join("nso.py")).expect("remove");
    assert!(matches!(
        Engine::with_table(&d, PatchTable::default()),
        Err(Fail::Unsupported(_))
    ));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn collected_nros_skip_refused_rows_and_need_every_file() {
    let d = std::env::temp_dir().join(format!("nxb-nro-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("dir");
    assert!(collected_nros(&d).is_none(), "no manifest yet");
    std::fs::write(
        d.join("nro.tsv"),
        "# name\tsha256\tsize\tnrr\twhere\nnro0\taa\t4\tx.nrr\tindex a\nrefused\tbb\t4\tx.nrr\tindex b\nnro1\tcc\t4\tx.nrr\tindex c\n",
    )
    .expect("tsv");
    assert!(collected_nros(&d).is_none(), "files missing");
    std::fs::write(d.join("nro0.nro"), b"x").expect("nro0");
    std::fs::write(d.join("nro1.nro"), b"x").expect("nro1");
    let v = collected_nros(&d).expect("complete");
    assert_eq!(
        v.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        ["nro0", "nro1"]
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_version_with_no_image_is_named_and_not_reported_as_an_nro_failure() {
    let d = std::env::temp_dir().join(format!("nxb-rewrite-noimage-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let profile = d.join("0100000000000000");
    let work = d.join("work");
    std::fs::create_dir_all(&profile).expect("profile");
    std::fs::create_dir_all(&work).expect("work");
    std::fs::write(profile.join("game.tsv"), "modules\tmain\n").expect("game.tsv");
    let e = ensure(&d, &profile, &work, "v720896").expect_err("no image");
    assert!(e.contains("no RomFS image of v720896"), "{e}");
    assert!(!e.contains("nro collection"), "{e}");
    let _ = std::fs::remove_dir_all(&d);
}
