//! Tests that load a model on the Neural Engine. They are ignored by default. Run them with
//! `NXKIT_COREML_PACKAGE=/path/to/model.mlpackage cargo test -p nxkit-coreml -- --ignored`.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::mpsc;

use nxkit_coreml::{Model, SharedTensor, os_build_tag};

fn load() -> Model {
    let package =
        PathBuf::from(std::env::var("NXKIT_COREML_PACKAGE").expect("NXKIT_COREML_PACKAGE"));
    let cache = std::env::temp_dir().join("nxkit-coreml-test-cache");
    let key = format!("test-{}", os_build_tag());
    Model::compile_and_load(&package, &cache, &key).expect("compile and load")
}

#[test]
#[ignore = "loads a model on the Neural Engine"]
fn plan_reports_every_operation() {
    let m = load();
    let report = m.compute_plan().expect("plan");
    println!("{report:?}");
    assert!(report.total > 0);
}

#[test]
#[ignore = "runs a prediction on the Neural Engine"]
fn predict_into_shared_tensors() {
    let m = load();
    let ins: Vec<(String, SharedTensor)> = m
        .inputs()
        .iter()
        .map(|i| (i.name.clone(), SharedTensor::new(&i.shape).expect("input")))
        .collect();
    let outs: Vec<(String, SharedTensor)> = m
        .outputs()
        .iter()
        .map(|o| (o.name.clone(), SharedTensor::new(&o.shape).expect("output")))
        .collect();
    for (_, t) in &ins {
        t.write_f16_bits(&vec![0x3800; t.len()]).expect("fill");
    }
    let (tx, rx) = mpsc::channel();
    let i: Vec<(&str, &SharedTensor)> = ins.iter().map(|(n, t)| (n.as_str(), t)).collect();
    let o: Vec<(&str, &SharedTensor)> = outs.iter().map(|(n, t)| (n.as_str(), t)).collect();
    m.predict_async(&i, &o, move |r| {
        let _ = tx.send(r);
    });
    rx.recv().expect("done").expect("prediction");
}
