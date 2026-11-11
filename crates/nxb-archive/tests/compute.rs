//! Compute pipelines in the archive: the shaders read from the tables, the plan with its reasons for leaving one out,
//! the script with `compute_pipeline_descriptors`, the batches and the streamed groups.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nxb_archive::compute::{
    COMPUTE_BATCH, chunks, plan_compute, read_computes, read_computes_with, summary,
};
use nxb_archive::plan::{archive_tsv, script_text, script_text_with};
use nxb_archive::script::Script;
use nxb_archive::stream::{Stream, StreamInputs};
use nxb_model::{Digest, sha256};
use std::collections::HashMap;
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

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(ToString::to_string).collect()
}

#[test]
fn computes_are_read_from_the_manifest_rows_then_the_map_rows() {
    let d = dir("compute_read");
    fs::write(
        d.join("manifest.tsv"),
        "# header\nS\tx\tps\nC\tb1_b\nC\t\nC\ta0_b\r\nC\tb1_b\n",
    )
    .unwrap();
    fs::write(
        d.join("map.tsv"),
        "k\tv\tcompute\tm1\nk\tv\tpixel\tm2\nk\tv\tcompute\ta0\nshort\tline\ncompute\nk\tv\tcompute\t\n",
    )
    .unwrap();
    let got = read_computes(
        &[d.join("map.tsv"), d.join("missing-map.tsv")],
        &[d.join("missing.tsv"), d.join("manifest.tsv")],
    );
    // the manifest's C rows first, a map row whose library is listed already is not repeated
    assert_eq!(got, strings(&["b1_b", "a0_b", "m1_b"]));
    assert!(read_computes(&[], &[]).is_empty());
}

#[test]
fn a_compute_shader_is_left_out_for_the_reason_the_host_would_not_find_it() {
    let d = dir("compute_plan");
    let ok = r#"{"chain": "abc", "stage": "cs", "entry": "go"}"#;
    library(&d, "ok_b", ok, Some(&[("go", false)]));
    // no stage and no entry in the reflection: the compute defaults, entry main0
    library(
        &d,
        "dflt_b",
        r#"{"chain": "abc"}"#,
        Some(&[("main0", false)]),
    );
    // the same library and entry once more
    library(&d, "ok_dup_b", ok, Some(&[("go", false)]));
    library(
        &d,
        "other_b",
        r#"{"chain": "zzz"}"#,
        Some(&[("main0", false)]),
    );
    library(
        &d,
        "vs_b",
        r#"{"chain": "abc", "stage": "vs"}"#,
        Some(&[("main0", false)]),
    );
    fs::write(d.join("bad_b.json"), "[1]").unwrap();
    library(&d, "nolib_b", r#"{"chain": "abc"}"#, None);
    library(&d, "noentry_b", ok, Some(&[("other", false)]));
    library(&d, "cnst_b", ok, Some(&[("go", true)]));
    let shaders = strings(&[
        "ok_b",
        "dflt_b",
        "ok_b",
        "other_b",
        "vs_b",
        "bad_b",
        "gone_b",
        "nolib_b",
        "noentry_b",
        "cnst_b",
    ]);
    let (pipes, left) = plan_compute(&d, "abc", &shaders);
    assert_eq!(
        pipes,
        vec![
            ("ok_b".to_string(), "go".to_string()),
            ("dflt_b".to_string(), "main0".to_string())
        ]
    );
    // reflection: other chain, stage vs, not an object, no file. library 1, entry 1, constants 1
    assert_eq!(left, [4, 1, 1, 1]);
    assert_eq!(
        summary(shaders.len(), pipes.len(), left),
        "10 compute shaders: 2 compute pipelines, left out: reflection 4, library 1, entry 1, constants 1"
    );
}

#[test]
fn the_script_has_compute_descriptors_only_when_there_are_computes() {
    let d = dir("compute_script");
    let bases = [(
        "v_b".to_string(),
        "main0".to_string(),
        "f_b".to_string(),
        "main0".to_string(),
    )];
    let none = script_text(&d, &bases);
    assert_eq!(none, script_text_with(&d, &bases, &[]));
    assert!(!none.contains("compute_pipeline_descriptors"));
    let pipes = [
        ("c1_b".to_string(), "main0".to_string()),
        ("v_b".to_string(), "main0".to_string()),
    ];
    let text = script_text_with(&d, &bases, &pipes);
    let s = Script::parse(&text).unwrap();
    assert_eq!((s.pipelines.len(), s.computes.len()), (1, 2));
    // a compute pipeline names the function alone, and a function used by both kinds is listed once
    assert!(text.contains("\"compute_function_descriptor\": \"fnd:c1_b.main0\""));
    assert_eq!(text.matches("\"label\": \"v_b.main0\"").count(), 1);
    assert_eq!(s.library_paths().unwrap().len(), 3);
    // the render key stays when only computes are given
    let only = script_text_with(&d, &[], &pipes);
    assert!(only.contains("\"render_pipeline_descriptors\": []"));
    assert!(Script::parse(&only).unwrap().pipelines.is_empty());
    // a script of render bases alone parses with no computes
    assert!(Script::parse(&none).unwrap().computes.is_empty());
}

fn digests(s: &Script) -> HashMap<String, Digest> {
    s.library_paths()
        .unwrap()
        .into_keys()
        .map(|l| {
            let d = sha256(l.as_bytes());
            (l, d)
        })
        .collect()
}

#[test]
fn compute_batches_hold_only_compute_pipelines_in_groups_of_sixteen() {
    let d = dir("compute_batches");
    let bases = [(
        "v_b".to_string(),
        "main0".to_string(),
        "f_b".to_string(),
        "main0".to_string(),
    )];
    let pipes: Vec<(String, String)> = (0..40)
        .map(|i| (format!("c{i}_b"), "main0".into()))
        .collect();
    let s = Script::parse(&script_text_with(&d, &bases, &pipes)).unwrap();
    let dg = digests(&s);
    let render = s.bases(&dg).unwrap();
    let compute = s.compute_bases(&dg).unwrap();
    assert_eq!((render.len(), compute.len()), (1, 40));
    assert!(compute.iter().all(|b| b.compute));
    assert!(render.iter().all(|b| !b.compute));
    assert!(compute.iter().all(|b| b.hash != render[0].hash));
    let groups = chunks(compute.clone());
    let sizes: Vec<usize> = groups.iter().map(Vec::len).collect();
    assert_eq!(sizes, [COMPUTE_BATCH, COMPUTE_BATCH, 8]);
    let text = s.batch_text(&groups[2]).unwrap();
    assert!(text.contains("\"render_pipeline_descriptors\": []"));
    assert_eq!(text.matches("compute_function_descriptor").count(), 8);
    assert!(text.contains("fnd:c39_b.main0"));
    assert!(!text.contains("v_b"));
    assert!(!text.contains("c0_b"));
    // a render batch has no compute key
    let r = s.batch_text(&render).unwrap();
    assert!(!r.contains("compute_pipeline_descriptors"));
    assert!(r.contains("vertex_function_descriptor"));
    // the hash follows the library's bytes and the entry
    let mut other = dg.clone();
    other.insert("c0_b".into(), sha256(b"changed"));
    let moved = s.compute_bases(&other).unwrap();
    assert_ne!(moved[0].hash, compute[0].hash);
    assert_eq!(moved[1].hash, compute[1].hash);
}

#[test]
fn archive_tsv_counts_the_compute_pipelines() {
    let t = archive_tsv(
        &["applegpu_g18p".to_string()],
        "macos",
        "27.0",
        "tt 1",
        5,
        2,
        7,
    );
    assert!(t.ends_with("bases\t5\nrest\t2\ncompute\t7\n"));
}

#[test]
fn streamed_groups_wait_for_their_compute_shaders() {
    let inp = StreamInputs {
        lib_dir: PathBuf::from("/nonexistent"),
        chain: "c".into(),
        pairs: vec![("v".into(), "f".into())],
        computes: (0..20).map(|i| format!("k{i}_b")).collect(),
        extras: Vec::new(),
        exacts: Vec::new(),
        zero: false,
        group_target: 8,
        limits: nxb_sched::Limits::new(1, 1, 0),
    };
    let s = Stream::new(&inp);
    // one pair group and two compute groups (16 and 4)
    assert_eq!(s.groups(), 3);
    let order = s.key_order();
    assert_eq!(order.len(), 22);
    assert!(order.contains(&"k19".to_string()));
    assert!(!order.iter().any(|k| k.ends_with("_b")));
}

#[test]
fn the_computes_the_host_recorded_follow_the_manifest_and_come_before_the_map() {
    let d = dir("compute_list");
    fs::write(d.join("manifest.tsv"), "C\tb1_b\n").unwrap();
    fs::write(d.join("map.tsv"), "k\tv\tcompute\tm1\nk\tv\tcompute\ta0\n").unwrap();
    // the host's list: `C <key> <stem> <entry>`; render rows, a stem that is not a `_b` library and short rows are skipped
    fs::write(
        d.join("seen.list"),
        "nxkit-pipelines-seen 2\nR\t1\t2\t0\tv_b\tmain0\tf_b\tmain0\t0\tTriangle\t1\t0\t-\t-\nC\t0000000000000001\ta0_b\tmain0\nC\t0000000000000002\tb1_b\tmain0\nC\t0000000000000003\tz9_b\tmain0\nC\t0000000000000004\tnot-a-lib\tmain0\nC\t5\n",
    )
    .unwrap();
    let got = read_computes_with(
        &[d.join("map.tsv")],
        &[d.join("manifest.tsv")],
        &[d.join("seen.list")],
    );
    assert_eq!(got, strings(&["b1_b", "a0_b", "z9_b", "m1_b"]));
    // no list: what `read_computes` gives
    assert_eq!(
        read_computes_with(&[d.join("map.tsv")], &[d.join("manifest.tsv")], &[]),
        read_computes(&[d.join("map.tsv")], &[d.join("manifest.tsv")])
    );
}
