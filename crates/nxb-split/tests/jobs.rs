//! The archive jobs against the reference (`data/jb_<case>/` holds the files of a tree with `/` written `__`,
//! `data/jb_<case>.exp` the jobs in order with their rows and the archives taken).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use nxb_model::sha256;
use nxb_split::SplitError;
use nxb_split::jobs::{Ctx, File, NroSet, deferred, plan_with};
use nxb_split::scanfile::finish;
use nxb_split::shaders::Profile;
use nxb_split::src::{Mem, Src};
use nxb_split::zstd::Dicts;

struct Own(Vec<u8>);
impl Src for Own {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> Result<(), SplitError> {
        Mem(&self.0).read_at(off, buf)
    }
}

fn run(dir: &str) -> String {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort_by_key(|n| n.replace("__", "/"));
    // `*.nro` files are the game's NROs (`DM_NRO_DIR`), not part of the tree
    let nros = NroSet {
        dir: "DM_NRO_DIR".into(),
        files: names
            .iter()
            .filter(|n| n.ends_with(".nro"))
            .map(|n| Arc::new(std::fs::read(format!("{dir}/{n}")).unwrap()))
            .collect(),
    };
    names.retain(|n| !n.ends_with(".nro"));
    let files: Vec<File> = names
        .iter()
        .map(|n| {
            let d = std::fs::read(format!("{dir}/{n}")).unwrap();
            let rel = n.replace("__", "/");
            let live = deferred(&rel, &d[..d.len().min(16)]);
            File {
                rel,
                src: Arc::new(Own(d)),
                live,
            }
        })
        .collect();
    if dir.contains("/jb_nlg_") {
        let i = files.iter().position(|f| f.rel.ends_with(".dict")).unwrap();
        let r = nxb_split::jobs::nlg::shaders(&files, i, &format!("/{}", files[i].rel))
            .and_then(|(f, v)| finish(&Profile::default(), f, &v));
        let mut out = String::new();
        match r {
            Err(_) => out += "E\n",
            Ok(None) => {}
            Ok(Some(r)) => {
                for (_, f) in &r.keys {
                    out += &format!(
                        "K\t{}\t{}\t{}\t{}\n",
                        f.place,
                        f.stage,
                        &sha256(&f.ctl).to_hex()[..16],
                        &sha256(&f.code).to_hex()[..16]
                    );
                }
                for (v, q, prog) in &r.pairs {
                    out += &format!("P\t{v}\t{q}\t{prog}\n");
                }
            }
        }
        return out;
    }
    let p = match plan_with(&files, &nros) {
        Ok(p) => p,
        Err(SplitError::Unsupported(_)) => return "UNSUP\n".into(),
        Err(_) => return "PLAN\n".into(),
    };
    let dicts = Dicts::new();
    let ctx = Ctx {
        files: &files,
        dicts: &dicts,
    };
    let prof = Profile::default();
    let mut out = String::new();
    for j in &p.jobs {
        out += &format!("J\t{}\n", j.rel);
        let r = (j.run)(&ctx, &mut Vec::new()).and_then(|o| finish(&prof, o.found, &o.var));
        match r {
            Err(SplitError::Unsupported(_)) => return "UNSUP\n".into(),
            Err(_) => out += "E\n",
            Ok(None) => {}
            Ok(Some(r)) => {
                for (_, f) in &r.keys {
                    out += &format!(
                        "K\t{}\t{}\t{}\t{}\n",
                        f.place,
                        f.stage,
                        &sha256(&f.ctl).to_hex()[..16],
                        &sha256(&f.code).to_hex()[..16]
                    );
                }
                for (v, q, prog) in &r.pairs {
                    out += &format!("P\t{v}\t{q}\t{prog}\n");
                }
            }
        }
    }
    let mut taken: Vec<&str> = p.taken.iter().map(|&i| files[i].rel.as_str()).collect();
    taken.sort_unstable();
    out + &format!("T\t{}\n", taken.join(","))
}

#[test]
fn archive_jobs_match_the_reference() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/");
    let mut cases: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| {
            e.unwrap().file_name().to_str().and_then(|n| {
                n.strip_prefix("jb_")?
                    .strip_suffix(".exp")
                    .map(str::to_string)
            })
        })
        .collect();
    cases.sort();
    assert!(!cases.is_empty());
    let mut bad = Vec::new();
    for c in cases {
        let want = std::fs::read_to_string(format!("{dir}jb_{c}.exp")).unwrap();
        let got = run(&format!("{dir}jb_{c}"));
        if got != want {
            bad.push(format!("{c}:\n want {want}\n got  {got}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn a_cra_archive_without_an_nro_table_fails_the_plan() {
    let arc = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/jb_cra_nonro/42"
    ))
    .unwrap();
    let files = [File {
        rel: "42".into(),
        src: Arc::new(Own(arc)),
        live: true,
    }];
    let Err(SplitError::Red(m)) = plan_with(&files, &NroSet::default()) else {
        panic!("the plan must fail");
    };
    assert_eq!(m, "/42: a CRA archive, and no NRO holds its file table");
    // a file that the hook did not keep back is not an archive
    let files = [File {
        live: false,
        ..files.into_iter().next().unwrap()
    }];
    assert!(
        plan_with(&files, &NroSet::default())
            .unwrap()
            .jobs
            .is_empty()
    );
}
