//! The batch keys are content keys: the bytes of the libraries a batch's bases name, their entries and descriptors, and
//! the tool identity. A rebuild with an unchanged library set finds every batch in the store; a changed library
//! changes only the batches with a base that names it.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use nxb_model::{Digest, sha256};
use nxb_pyjson::Value;

use crate::cut::cut;
use crate::script::Script;
use crate::{Config, batch_key};

fn config(toolchain: &str) -> Config {
    Config {
        script: PathBuf::new(),
        arch: "applegpu_g14s".into(),
        platform: "macos".into(),
        osmin: "27.0".into(),
        sdkroot: PathBuf::new(),
        metal_tt: PathBuf::new(),
        metal_pack: PathBuf::new(),
        toolchain: toolchain.into(),
        store: PathBuf::new(),
        work: PathBuf::new(),
        out: PathBuf::new(),
        target: 8,
        tokens: 1,
        cost: 1,
        mem_per_batch: 0,
        mem_budget: 0,
        pack_threads: 1,
        direct: None,
        nice: 0,
        progress: None,
        merge: true,
    }
}

/// A script of `n` vertex libraries `v<i>_b` (in folder `dir`), each drawn with three pixel libraries.
fn script(n: usize, dir: &str) -> Script {
    let (mut libs, mut fns, mut pipes) = (Vec::new(), Vec::new(), Vec::new());
    let lib = |name: String, libs: &mut Vec<Value>, fns: &mut Vec<Value>, entry: &str| {
        libs.push(Value::obj([
            ("label", Value::str(&name)),
            ("path", Value::str(format!("{dir}/{name}.metallib"))),
        ]));
        fns.push(Value::obj([
            ("label", Value::str(&name)),
            ("library", Value::str(format!("lib:{name}"))),
            ("name", Value::str(entry)),
        ]));
    };
    for f in 0..3 {
        lib(format!("f{f}_b"), &mut libs, &mut fns, "main0");
    }
    for v in 0..n {
        lib(format!("v{v}_b"), &mut libs, &mut fns, "main0");
        for f in 0..3 {
            pipes.push(Value::obj([
                (
                    "vertex_function_descriptor",
                    Value::str(format!("fnd:v{v}_b")),
                ),
                (
                    "fragment_function_descriptor",
                    Value::str(format!("fnd:f{f}_b")),
                ),
                (
                    "color_attachments",
                    Value::List(vec![Value::str("BGRA8Unorm")]),
                ),
            ]));
        }
    }
    let v = Value::obj([
        ("libraries", Value::List(libs)),
        (
            "function_descriptors",
            Value::obj([("library_function_descriptors", Value::List(fns))]),
        ),
        (
            "pipeline_descriptors",
            Value::obj([("render_pipeline_descriptors", Value::List(pipes))]),
        ),
    ]);
    Script::parse(&nxb_pyjson::dumps(&v)).expect("script")
}

/// The library digests: the bytes of each library are its name, `changed` gets other bytes.
fn digests(s: &Script, changed: Option<&str>) -> HashMap<String, Digest> {
    s.library_paths()
        .expect("paths")
        .into_keys()
        .map(|l| {
            let bytes = if Some(l.as_str()) == changed {
                format!("{l} new")
            } else {
                l.clone()
            };
            (l, sha256(bytes.as_bytes()))
        })
        .collect()
}

fn keys(c: &Config, s: &Script, d: &HashMap<String, Digest>) -> BTreeSet<String> {
    cut(s.bases(d).expect("bases"), c.target)
        .iter()
        .map(|b| batch_key(c, b).to_hex())
        .collect()
}

#[test]
fn equal_libraries_in_another_folder_keep_every_batch_key() {
    let c = config("metal-tt 32023.922");
    let (a, b) = (
        script(40, "/w/b-a685cbe682ee"),
        script(40, "/w/b-71c2f44e5744"),
    );
    let ka = keys(&c, &a, &digests(&a, None));
    assert!(ka.len() > 4, "{} batches", ka.len());
    assert_eq!(ka, keys(&c, &b, &digests(&b, None)));
}

#[test]
fn a_changed_library_changes_only_the_batches_that_name_it() {
    let c = config("metal-tt 32023.922");
    let s = script(40, "/w/b");
    let before = keys(&c, &s, &digests(&s, None));
    let after = keys(&c, &s, &digests(&s, Some("v7_b")));
    // its bases move to the hash range of the new bytes: the batch it left and the batch it joined change
    let changed = before.difference(&after).count();
    assert!(
        (1..=2).contains(&changed),
        "{changed} of {} batches",
        before.len()
    );
    // a pixel library drawn with every vertex library is in every batch
    let all = keys(&c, &s, &digests(&s, Some("f1_b")));
    assert_eq!(before.intersection(&all).count(), 0);
}

#[test]
fn another_toolchain_changes_every_batch_key() {
    let s = script(40, "/w/b");
    let d = digests(&s, None);
    let a = keys(&config("metal-tt 32023.922"), &s, &d);
    let b = keys(&config("metal-tt 32024.100"), &s, &d);
    assert_eq!(a.intersection(&b).count(), 0);
}
