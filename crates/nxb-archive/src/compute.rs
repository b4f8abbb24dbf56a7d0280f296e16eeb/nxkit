//! Compute pipelines in the archive (`pso_offline.py`: `read_computes` and `plan_compute`). The host looks a compute
//! pipeline up by its function alone (`cps_bundle` in `nvn_mtl.m`: no threadgroup size, no indirect command buffers),
//! so a compute pipeline descriptor names one function. Compute pipelines are compiled in small groups of
//! [`COMPUTE_BATCH`], apart from the render bases, so a change on one side leaves the other side's batches in the store.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use nxb_pyjson::Value;

use crate::cut::Base;
use crate::plan::functions;

/// Compute pipelines per compiler run. One run keeps two or three cores busy, so small groups run side by side.
pub const COMPUTE_BATCH: usize = 16;

/// One compute pipeline: (library, entry).
pub type ComputePipe = (String, String);

/// Why compute shaders were left out: reflection, library, entry, function constants.
pub type ComputeLeft = [usize; 4];

/// `read_computes`: the compute shaders by the library the host loads each from. The prewarm lists' `C` rows come
/// first, in their order. The shader maps' rows of stage `compute` follow (the key is column 4, with `_b` added).
/// Each library is listed once. Missing files are skipped.
pub fn read_computes(maps: &[PathBuf], manifests: &[PathBuf]) -> Vec<String> {
    read_computes_with(maps, manifests, &[])
}

/// [`read_computes`] and the compute pipelines that the host recorded: the `C` rows of its pipeline lists
/// (`C <key> <stem> <entry>`, `nxkit-gpu` `pipelines/record.rs`) follow the manifests' rows, before the shader maps'.
pub fn read_computes_with(
    maps: &[PathBuf],
    manifests: &[PathBuf],
    lists: &[PathBuf],
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |name: String| {
        if seen.insert(name.clone()) {
            out.push(name);
        }
    };
    let lines = |p: &Path| -> Vec<String> {
        if !p.is_file() {
            return Vec::new();
        }
        fs::read(p)
            .map(|b| {
                String::from_utf8_lossy(&b)
                    .split('\n')
                    .map(|l| l.trim_end_matches('\r').to_string())
                    .collect()
            })
            .unwrap_or_default()
    };
    for p in manifests {
        for l in lines(p) {
            let c: Vec<&str> = l.split('\t').collect();
            if c.len() >= 2 && c[0] == "C" && !c[1].is_empty() {
                add(c[1].to_string());
            }
        }
    }
    for p in lists {
        for l in lines(p) {
            let c: Vec<&str> = l.split('\t').collect();
            if c.len() == 4 && c[0] == "C" && c[2].ends_with("_b") {
                add(c[2].to_string());
            }
        }
    }
    for p in maps {
        for l in lines(p) {
            let c: Vec<&str> = l.split('\t').collect();
            if c.len() > 3 && c[2] == "compute" && !c[3].is_empty() {
                add(format!("{}_b", c[3]));
            }
        }
    }
    out
}

/// `plan_compute`: the compute pipelines of `shaders` (libraries `<key>_b`), each with its reflection's entry, and the
/// shaders left out because the host would not find their pipeline.
pub fn plan_compute(
    lib_dir: &Path,
    chain: &str,
    shaders: &[String],
) -> (Vec<ComputePipe>, ComputeLeft) {
    let mut pipes = Vec::new();
    let mut seen = HashSet::new();
    let mut left: ComputeLeft = [0; 4];
    for pc in shaders {
        let j = fs::read(lib_dir.join(format!("{pc}.json")))
            .ok()
            .and_then(|t| nxb_pyjson::parse(&String::from_utf8_lossy(&t)).ok());
        let ok = |j: &Value| {
            matches!(j, Value::Object(_))
                && j.get("chain").and_then(Value::as_str) == Some(chain)
                && match j.get("stage") {
                    None => true,
                    Some(s) => s.as_str() == Some("cs"),
                }
        };
        let Some(j) = j.filter(ok) else {
            left[0] += 1;
            continue;
        };
        let entry = j
            .get("entry")
            .and_then(Value::as_str)
            .filter(|e| !e.is_empty())
            .unwrap_or("main0")
            .to_string();
        let Some(fs) = functions(&lib_dir.join(format!("{pc}.metallib"))) else {
            left[1] += 1;
            continue;
        };
        let Some(constants) = fs.iter().rev().find(|(n, _)| *n == entry).map(|x| x.1) else {
            left[2] += 1;
            continue;
        };
        if constants {
            left[3] += 1;
            continue;
        }
        if seen.insert((pc.clone(), entry.clone())) {
            pipes.push((pc.clone(), entry));
        }
    }
    (pipes, left)
}

/// The compute bases cut into groups of [`COMPUTE_BATCH`], in script order.
pub fn chunks(bases: Vec<Base>) -> Vec<Vec<Base>> {
    bases.chunks(COMPUTE_BATCH).map(<[Base]>::to_vec).collect()
}

/// The line the plan prints: how many compute shaders there were, how many pipelines and why the rest is left out.
pub fn summary(shaders: usize, pipes: usize, left: ComputeLeft) -> String {
    format!(
        "{shaders} compute shaders: {pipes} compute pipelines, left out: reflection {}, library {}, entry {}, constants {}",
        left[0], left[1], left[2], left[3]
    )
}
