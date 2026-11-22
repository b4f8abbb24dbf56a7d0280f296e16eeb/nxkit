//! Parity of the engine detection with the title modules, over game dumps on the machine of the person who runs it.
//!
//! `NXB_PARITY_ENGINE` names a folder with one folder per title: `<TITLE ID>/exefs/<module>` (the NSO files) and
//! `<TITLE ID>/romfs/` (the RomFS, loose). The test is `#[ignore]` and never uses a default path. For every title it
//! detects the family from the markers and compares it with `Title::engine`, then prints a confusion list. A title with
//! no module is listed with its detection. Nothing of a game is written anywhere.

use super::engine_scan;
use nxtitle::{TitleFacts, TitleId};
use std::path::{Path, PathBuf};

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(root, &p, out);
        } else if let Ok(rel) = p.strip_prefix(root) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
}

#[test]
#[ignore = "reads game dumps named by NXB_PARITY_ENGINE"]
fn the_detected_engine_is_the_declared_one() {
    let Some(root) = std::env::var_os("NXB_PARITY_ENGINE").map(PathBuf::from) else {
        eprintln!("NXB_PARITY_ENGINE is not set: skipped");
        return;
    };
    let catalog = nxtitles::catalog().unwrap();
    let (mut same, mut confusion, mut undecided) = (0, Vec::new(), Vec::new());
    for entry in std::fs::read_dir(&root).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(id) = u64::from_str_radix(&name, 16) else {
            continue;
        };
        let mut ro = Vec::new();
        for m in std::fs::read_dir(entry.path().join("exefs"))
            .into_iter()
            .flatten()
            .flatten()
        {
            let Ok(data) = std::fs::read(m.path()) else {
                continue;
            };
            let fname = m.file_name().to_string_lossy().into_owned();
            if let Ok(module) = nxb_rewrite::nso::load(&data, &fname) {
                let (off, size) = module.segments[1];
                if let Some(r) = module.image.get(off..off.saturating_add(size)) {
                    ro.push(r.to_vec());
                }
            }
        }
        let romfs = entry.path().join("romfs");
        let mut paths = Vec::new();
        walk(&romfs, &romfs, &mut paths);
        let hits = engine_scan::hits(
            &catalog,
            ro.iter().map(Vec::as_slice),
            paths.iter().map(String::as_str),
        );
        let mut facts = TitleFacts::new(TitleId(id));
        facts.libraries = hits.iter().map(|s| s.to_string()).collect();
        let d = catalog.detect_engine(&facts);
        let declared = catalog
            .titles()
            .iter()
            .find(|t| t.id() == TitleId(id))
            .map(|t| t.engine());
        match (d.family, declared) {
            (Some(a), Some(b)) if a == b => same += 1,
            (Some(a), declared) => confusion.push(format!(
                "{name}: detected {a}, declared {declared:?}, markers {hits:?}"
            )),
            (None, declared) => undecided.push(format!(
                "{name}: none ({:?}), declared {declared:?}, markers {hits:?}",
                d.candidates
            )),
        }
    }
    eprintln!("same: {same}");
    for c in &confusion {
        eprintln!("CONFUSED {c}");
    }
    for u in &undecided {
        eprintln!("undecided {u}");
    }
    // a wrong answer is a failure, no answer is not
    assert!(
        confusion.is_empty(),
        "{} titles detected as another family",
        confusion.len()
    );
}
