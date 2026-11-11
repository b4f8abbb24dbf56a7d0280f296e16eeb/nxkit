//! Recorded states as bases: which ones the plan accepts, and the script they make.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nxb_archive::plan::{plan, script_text_all};
use nxb_archive::script::Script;
use nxb_archive::seen::{read_list, usable};
use std::fs;
use std::path::{Path, PathBuf};

fn dir(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn tag(out: &mut Vec<u8>, t: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(t);
    out.extend_from_slice(&(data.len() as u16).to_le_bytes());
    out.extend_from_slice(data);
}

/// A metallib with only the tables `functions` reads: the entry names, and for each whether it has function constants.
fn metallib(entries: &[(&str, bool)]) -> Vec<u8> {
    let mut names = (entries.len() as u32).to_le_bytes().to_vec();
    let mut data = Vec::new();
    for (n, constants) in entries {
        let mut t = Vec::new();
        tag(&mut t, b"NAME", n.as_bytes());
        t.extend_from_slice(b"ENDT");
        names.extend_from_slice(&(t.len() as u32 + 4).to_le_bytes());
        names.extend_from_slice(&t);
        let mut d = Vec::new();
        if *constants {
            tag(&mut d, b"CNST", &[]);
        }
        d.extend_from_slice(b"ENDT");
        data.extend_from_slice(&(d.len() as u32).to_le_bytes());
        data.extend_from_slice(&d);
    }
    let fo = 0x58usize;
    let po = fo + names.len();
    let mut b = vec![0u8; fo];
    b[..4].copy_from_slice(b"MTLB");
    b[0x18..0x20].copy_from_slice(&(fo as u64).to_le_bytes());
    b[0x28..0x30].copy_from_slice(&(po as u64).to_le_bytes());
    b[0x30..0x38].copy_from_slice(&(data.len() as u64).to_le_bytes());
    b.extend_from_slice(&names);
    b.extend_from_slice(&data);
    b
}

fn library(d: &Path, name: &str, reflection: &str, entries: Option<&[(&str, bool)]>) {
    fs::write(d.join(format!("{name}.json")), reflection).unwrap();
    if let Some(e) = entries {
        fs::write(d.join(format!("{name}.metallib")), metallib(e)).unwrap();
    }
}

fn state_list(vstem: &str, fstem: &str) -> String {
    format!(
        "nxkit-pipelines-seen 2\nR\t1\t2\t0\t{vstem}\tmain0\t{fstem}\tmain0\t0\tTriangle\t1\t0\tBgra8Unorm:f:-\ta0.Float3.0.0,l0.12.PerVertex.1\n"
    )
}

#[test]
fn a_vertex_shader_with_stage_inputs_gets_a_base_only_for_its_recorded_state() {
    let d = dir("seen_usable");
    library(
        &d,
        "v1_b",
        r#"{"chain": "abc", "entry": "main0", "vertex_inputs": [{"slot": 0}]}"#,
        Some(&[("main0", false)]),
    );
    library(
        &d,
        "f1_b",
        r#"{"chain": "abc", "entry": "main0"}"#,
        Some(&[("main0", false)]),
    );
    let pairs = vec![("v1".to_string(), "f1".to_string())];
    // the plain plan leaves the pair out: its vertex function needs the layout of the draw
    let p = plan(&d, "abc", &pairs, false);
    assert!(p.bases.is_empty());
    assert_eq!(p.left[2], 1);
    let states = read_list(&state_list("v1_b", "f1_b"));
    assert_eq!(states.len(), 1);
    let (ok, _) = usable(&d, "abc", &states, false);
    assert_eq!(ok, states);
    // the script holds the base with the layout
    let s = Script::parse(&script_text_all(&d, &[], &ok, &[])).unwrap();
    assert_eq!(s.pipelines.len(), 1);
    assert!(s.pipelines[0].get("vertex_descriptor").is_some());
    let labels: Vec<String> = s.library_paths().unwrap().into_keys().collect();
    assert_eq!(labels, ["f1_b", "v1_b"]);
}

#[test]
fn a_state_the_libraries_cannot_serve_is_left_out() {
    let d = dir("seen_unusable");
    library(
        &d,
        "v1_b",
        r#"{"chain": "abc", "entry": "main0"}"#,
        Some(&[("main0", false)]),
    );
    library(
        &d,
        "f1_b",
        r#"{"chain": "abc", "entry": "main0"}"#,
        Some(&[("main0", false)]),
    );
    // function constants: the device specializes the pair
    library(
        &d,
        "v2_b",
        r#"{"chain": "abc", "entry": "main0"}"#,
        Some(&[("main0", true)]),
    );
    library(
        &d,
        "f2_b",
        r#"{"chain": "abc", "entry": "main0"}"#,
        Some(&[("main0", false)]),
    );
    // another chain
    library(
        &d,
        "v3_b",
        r#"{"chain": "zzz", "entry": "main0"}"#,
        Some(&[("main0", false)]),
    );
    let mut all = String::new();
    for (v, f) in [
        ("v1_b", "f1_b"),
        ("v2_b", "f2_b"),
        ("v3_b", "f1_b"),
        ("gone_b", "f1_b"),
    ] {
        all.push_str(&state_list(v, f));
    }
    let states = read_list(&all);
    assert_eq!(states.len(), 4);
    let (ok, _) = usable(&d, "abc", &states, false);
    assert_eq!(ok.len(), 1);
    assert_eq!(ok[0].vertex.0, "v1_b");
    // a missing entry
    let none = read_list(&state_list("v1_b", "f1_b").replace("\tmain0\t0", "\tother\t0"));
    assert!(usable(&d, "abc", &none, false).0.is_empty());
}

/// By hand, against the files of a real build: `NXKIT_SEEN_WORK=<work folder> NXKIT_SEEN_LIST=<list> cargo test -p
/// nxb-archive --test seen real_build -- --ignored --nocapture`.
#[test]
#[ignore = "needs a build's work folder and a list"]
fn real_build() {
    let (Ok(work), Ok(list)) = (
        std::env::var("NXKIT_SEEN_WORK"),
        std::env::var("NXKIT_SEEN_LIST"),
    ) else {
        return;
    };
    let lib = fs::read_dir(PathBuf::from(&work).join("shader/cache"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("b-"))
        })
        .unwrap();
    let chain = lib
        .file_name()
        .unwrap()
        .to_string_lossy()
        .trim_start_matches("b-")
        .to_string();
    let states = nxb_archive::seen::read_lists(&[PathBuf::from(list)]);
    let (ok, refused) = usable(&lib, &chain, &states, false);
    eprintln!(
        "states {} usable {} refused {}",
        states.len(),
        ok.len(),
        refused.len()
    );
    for r in refused.iter().take(3) {
        eprintln!("  {}: {}", r.what, r.reason);
    }
}

#[test]
fn a_pull_shader_gets_its_stage_input_base_from_the_b_library() {
    let d = dir("seen_pull");
    // the host draws this shader through `_b` with a vertex layout unless pulling is on; `_bp` is the plain base
    library(
        &d,
        "v1_b",
        r#"{"chain": "abc", "entry": "main0", "vertex_inputs": [{"slot": 0}], "pull": {"slot": 16, "entry": "main0"}}"#,
        Some(&[("main0", false)]),
    );
    library(&d, "v1_bp", "{}", Some(&[("main0", false)]));
    library(
        &d,
        "f1_b",
        r#"{"chain": "abc", "entry": "main0"}"#,
        Some(&[("main0", false)]),
    );
    let pairs = vec![("v1".to_string(), "f1".to_string())];
    assert_eq!(plan(&d, "abc", &pairs, false).bases[0].0, "v1_bp");
    let states = read_list(&state_list("v1_b", "f1_b"));
    assert_eq!(usable(&d, "abc", &states, false).0, states);
}
