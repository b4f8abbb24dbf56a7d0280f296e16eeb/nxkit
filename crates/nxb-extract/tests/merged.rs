//! Merged packages (base game + update + DLC in one NSP, a card with two games): which NCAs are the game's, as
//! `nx_extract`'s `OpenNsp` with a content meta type and `--app` picks them.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use std::sync::Arc;

use common::*;
use nxb_extract::cache::BlockCache;
use nxb_extract::keys::KeySet;
use nxb_extract::meta::{APPLICATION, PATCH, filter};
use nxb_extract::nca::{ContentType, open_package, pick};
use nxb_extract::storage::{FileStorage, Storage};
use nxb_extract::{Request, extract};

const APP: u64 = 0x0100_0000_0000_2000;
const OTHER: u64 = 0x0100_0000_0000_1000;

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
        html: false,
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

fn ids(n: &[nxb_extract::nca::Nca]) -> Vec<String> {
    let mut v: Vec<String> = n.iter().map(|x| x.id.clone()).collect();
    v.sort();
    v
}

#[test]
fn a_merged_nsp_gives_the_base_game_and_the_update_their_own_ncas() {
    let dir = tmp("merged");
    let keys = write_keys(&dir);
    let base = title(0x80, APP, 0, 1, true);
    let upd = title(0x81, APP, 65536, 2, true);
    // the add-on content: its own title ID above the game's
    let dlc = title(0x82, APP + 0x1001, 65536, 3, false);
    let mut all = base.files.clone();
    all.extend(upd.files.clone());
    all.extend(dlc.files.clone());
    std::fs::write(dir.join("m.nsp"), pfs0(&all)).unwrap();
    std::fs::write(dir.join("b.nsp"), pfs0(&base.files)).unwrap();

    let mut ks = KeySet::parse(&keys_text());
    let pkg: Arc<dyn Storage> = Arc::new(FileStorage::open(&dir.join("m.nsp")).unwrap());
    let ncas = open_package(&mut ks, pkg, "m.nsp").unwrap();
    assert_eq!(ncas.len(), 8);
    let cache = Arc::new(BlockCache::new(8));
    let names = |t: &Title| {
        let mut v: Vec<String> = t
            .files
            .iter()
            .filter(|f| !f.0.ends_with(".cnmt.nca"))
            .map(|f| f.0.split('.').next().unwrap().to_string())
            .collect();
        v.sort();
        v
    };
    // as base game: its Program and Control NCAs and its meta, the update's and the add-on's are left out
    let take = |ncas: Vec<nxb_extract::nca::Nca>, kind, title| {
        let mut ks = KeySet::parse(&keys_text());
        let _ = &mut ks;
        filter(&ks, ncas, kind, title, "m.nsp", &cache).unwrap()
    };
    let ncas2 = {
        let pkg: Arc<dyn Storage> = Arc::new(FileStorage::open(&dir.join("m.nsp")).unwrap());
        open_package(&mut ks, pkg, "m.nsp").unwrap()
    };
    let (kept, log) = take(ncas, APPLICATION, None);
    assert_eq!(ids(&kept), names(&base));
    assert!(
        log.starts_with(
            "leaving out 3 NCAs of m.nsp that its Application content meta does not list"
        ),
        "{log}"
    );
    // as update: the Patch meta of the application's title ID | 0x800
    let (kept, log) = take(ncas2, PATCH, Some(APP | 0x800));
    assert_eq!(ids(&kept), names(&upd));
    assert!(
        log.contains("leaving out 3 NCAs of m.nsp that its Patch content meta"),
        "{log}"
    );
    let (prog, ctrl) = (
        pick(&kept, ContentType::Program, None).unwrap(),
        pick(&kept, ContentType::Control, Some(APP)),
    );
    assert_eq!(prog.title_id, APP);
    assert!(ctrl.is_some());

    // extraction from the merged NSP equals extraction from the base game's NSP alone
    extract(&request(&keys, &dir.join("b.nsp"), &dir.join("a"))).unwrap();
    extract(&request(&keys, &dir.join("m.nsp"), &dir.join("b"))).unwrap();
    same_outputs(&dir.join("a"), &dir.join("b"));
    // the engine's extractor on the same merged package
    let mut bu = base.files.clone();
    bu.extend(upd.files.clone());
    std::fs::write(dir.join("bu.nsp"), pfs0(&bu)).unwrap();
    if let Some(text) = oracle_extract(&keys, &dir.join("bu.nsp"), &dir.join("c"), &[]) {
        same_outputs(&dir.join("a"), &dir.join("c"));
        assert!(
            text.contains("leaving out 2 NCAs of bu.nsp that its Application content meta"),
            "{text}"
        );
    }
    cleanup(&dir);
}

#[test]
fn a_card_with_two_games_takes_the_one_named_by_app_else_the_lowest() {
    let dir = tmp("two-games");
    let keys = write_keys(&dir);
    let a = title(0x80, APP, 0, 1, true);
    let b = title(0x80, OTHER, 0, 4, true);
    let mut all = a.files.clone();
    all.extend(b.files.clone());
    let card = xci(&[("secure", all)]);
    std::fs::write(dir.join("two.xci"), card).unwrap();
    std::fs::write(dir.join("a.nsp"), pfs0(&a.files)).unwrap();
    std::fs::write(dir.join("b.nsp"), pfs0(&b.files)).unwrap();
    let mut r = request(&keys, &dir.join("two.xci"), &dir.join("lowest"));
    extract(&r).unwrap();
    extract(&request(&keys, &dir.join("b.nsp"), &dir.join("expect-b"))).unwrap();
    same_outputs(&dir.join("expect-b"), &dir.join("lowest"));
    r.app = Some(APP);
    r.out_base = dir.join("named");
    extract(&r).unwrap();
    extract(&request(&keys, &dir.join("a.nsp"), &dir.join("expect-a"))).unwrap();
    same_outputs(&dir.join("expect-a"), &dir.join("named"));
    // an application that is not on the card: an error that names the ones that are
    r.app = Some(0x0100_0000_0000_3000);
    r.out_base = dir.join("none");
    let e = extract(&r).err().unwrap().to_string();
    assert!(
        e.contains("has no Application content meta of title 0100000000003000"),
        "{e}"
    );
    assert!(
        e.contains("0100000000002000") && e.contains("0100000000001000"),
        "{e}"
    );
    if let Some(text) = oracle_extract(
        &keys,
        &dir.join("two.xci"),
        &dir.join("c"),
        &["--app", "0100000000002000"],
    ) {
        same_outputs(&dir.join("expect-a"), &dir.join("c"));
        assert!(text.contains("title 0100000000002000"), "{text}");
    }
    cleanup(&dir);
}

#[test]
fn a_program_of_a_multi_program_application_is_picked_by_its_title_id() {
    let dir = tmp("programs");
    let keys = write_keys(&dir);
    // one package, two programs (title IDs APP and APP + 1) as the programs of one application
    let (p0, p1) = (
        title(0x80, APP, 0, 1, true),
        title(0x80, APP + 1, 0, 2, true),
    );
    let mut all = p0.files.clone();
    all.extend(p1.files.clone());
    std::fs::write(dir.join("m.nsp"), pfs0(&all)).unwrap();
    std::fs::write(dir.join("a.nsp"), pfs0(&p0.files)).unwrap();
    std::fs::write(dir.join("b.nsp"), pfs0(&p1.files)).unwrap();

    let mut r = request(&keys, &dir.join("m.nsp"), &dir.join("lowest"));
    extract(&r).unwrap();
    extract(&request(&keys, &dir.join("a.nsp"), &dir.join("expect-a"))).unwrap();
    same_outputs(&dir.join("expect-a"), &dir.join("lowest"));

    r.program = Some(APP + 1);
    r.out_base = dir.join("second");
    extract(&r).unwrap();
    extract(&request(&keys, &dir.join("b.nsp"), &dir.join("expect-b"))).unwrap();
    same_outputs(&dir.join("expect-b"), &dir.join("second"));

    r.program = Some(APP + 2);
    r.out_base = dir.join("none");
    let e = extract(&r).err().unwrap().to_string();
    assert!(
        e.contains("no Program NCA 0100000000002002 in the base NSP"),
        "{e}"
    );
    if let Some(text) = oracle_extract(
        &keys,
        &dir.join("m.nsp"),
        &dir.join("c"),
        &["--program=0100000000002001"],
    ) {
        same_outputs(&dir.join("expect-b"), &dir.join("c"));
        assert!(text.contains("title 0100000000002001"), "{text}");
    }
    cleanup(&dir);
}
