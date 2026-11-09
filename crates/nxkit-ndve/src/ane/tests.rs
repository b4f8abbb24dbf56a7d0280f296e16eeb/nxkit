//! Structure tests of the package on synthetic weights, and the check against coremltools (ignored by default).

use super::*;
use crate::half;
use crate::net::{BLOB_LEN, RESAMPLER_LEN};

/// Deterministic pseudo-random f16 bytes in about [-0.2, 0.2].
fn synth(len: usize, seed: u64) -> Vec<u8> {
    let mut s = seed;
    (0..len / 2)
        .flat_map(|_| {
            s = s
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let f = ((s >> 40) as f32 / 16_777_216.0 - 0.5) * 0.4;
            half::from_f32(f).to_le_bytes()
        })
        .collect()
}

fn full() -> Package {
    build_package(&synth(BLOB_LEN, 1), &synth(RESAMPLER_LEN, 2), Variant::Full).expect("builds")
}

fn count(p: &Package, kind: &str) -> usize {
    p.ops.iter().filter(|o| o.kind == kind).count()
}

fn shape_of<'a>(p: &'a Package, name: &str) -> &'a [usize] {
    &p.ops
        .iter()
        .find(|o| o.output == name)
        .expect("op exists")
        .shape
}

#[test]
fn the_full_model_has_50_operations() {
    let p = full();
    assert_eq!(p.ops.len(), 50);
    let kinds: Vec<(&str, usize)> = [
        "conv",
        "conv_transpose",
        "relu",
        "add",
        "clip",
        "pad",
        "pixel_shuffle",
        "pixel_unshuffle",
        "slice_by_size",
        "identity",
    ]
    .iter()
    .map(|k| (*k, count(&p, k)))
    .collect();
    assert_eq!(
        kinds,
        [
            ("conv", 18),
            ("conv_transpose", 2),
            ("relu", 16),
            ("add", 5),
            ("clip", 1),
            ("pad", 1),
            ("pixel_shuffle", 2),
            ("pixel_unshuffle", 2),
            ("slice_by_size", 1),
            ("identity", 2),
        ]
    );
}

#[test]
fn the_unet_only_model_has_33_operations() {
    let p = build_package(&synth(BLOB_LEN, 1), &[], Variant::UnetOnly).expect("builds");
    assert_eq!(p.ops.len(), 33);
    assert_eq!(p.outputs.len(), 1);
    assert_eq!(p.outputs[0].name, "final");
    assert_eq!(p.ops.last().map(|o| o.output.as_str()), Some("final"));
}

#[test]
fn names_and_shapes() {
    let p = full();
    assert_eq!(
        p.input,
        TensorSpec {
            name: "x".into(),
            shape: vec![1, 4, 1080, 1920]
        }
    );
    assert_eq!(
        p.outputs,
        [
            TensorSpec {
                name: "delta".into(),
                shape: vec![1, 1, 1440, 2560]
            },
            TensorSpec {
                name: "final".into(),
                shape: vec![1, 3, 1080, 1920]
            },
        ]
    );
    assert_eq!(p.ops[0].kind, "pixel_unshuffle");
    assert_eq!(shape_of(&p, "pixel_unshuffle_0"), [1, 16, 540, 960]);
    assert_eq!(shape_of(&p, "conv_2"), [1, 32, 270, 480]);
    assert_eq!(shape_of(&p, "conv_4"), [1, 64, 135, 240]);
    assert_eq!(
        shape_of(&p, "conv_transpose_0_has_output_shape"),
        [1, 32, 270, 480]
    );
    assert_eq!(
        shape_of(&p, "conv_transpose_1_has_output_shape"),
        [1, 16, 540, 960]
    );
    assert_eq!(shape_of(&p, "conv_9"), [1, 12, 540, 960]);
    assert_eq!(shape_of(&p, "pixel_shuffle_0"), [1, 3, 1080, 1920]);
    assert_eq!(shape_of(&p, "clip_0"), [1, 3, 1080, 1920]);
    assert_eq!(shape_of(&p, "conv_10"), [1, 1, 1080, 1920]);
    assert_eq!(shape_of(&p, "pad_0_cast_fp16"), [1, 1, 1092, 1932]);
    assert_eq!(shape_of(&p, "pixel_unshuffle_1"), [1, 36, 182, 322]);
    assert_eq!(shape_of(&p, "conv_11"), [1, 192, 180, 322]);
    assert_eq!(shape_of(&p, "conv_12"), [1, 256, 180, 320]);
    assert_eq!(shape_of(&p, "conv_16"), [1, 256, 180, 320]);
    assert_eq!(shape_of(&p, "conv_17"), [1, 64, 180, 320]);
    assert_eq!(shape_of(&p, "pixel_shuffle_1"), [1, 1, 1440, 2560]);
    let last: Vec<&str> = p
        .ops
        .iter()
        .rev()
        .take(2)
        .map(|o| o.output.as_str())
        .collect();
    assert_eq!(last, ["final", "delta"]);
}

#[test]
fn the_weight_file_is_aligned_and_has_the_offsets_of_the_prototype() {
    let p = full();
    let w = p.file(package::WEIGHTS_PATH).expect("weights");
    assert_eq!(p.weights.len(), 31);
    assert_eq!(u32::from_le_bytes([w[0], w[1], w[2], w[3]]), 31);
    for e in &p.weights {
        assert_eq!(e.offset % 64, 0, "{}", e.name);
        assert_eq!(&w[e.offset..e.offset + 4], &[0xef, 0xbe, 0xad, 0xde]);
        let size = u64::from_le_bytes(w[e.offset + 8..e.offset + 16].try_into().expect("8"));
        let at = u64::from_le_bytes(w[e.offset + 16..e.offset + 24].try_into().expect("8"));
        assert_eq!(size as usize, e.len, "{}", e.name);
        assert_eq!(at as usize, e.offset + 64, "{}", e.name);
    }
    let at = |n: &str| p.weights.iter().find(|e| e.name == n).map(|e| e.offset);
    // The offsets coremltools gave the same model.
    assert_eq!(at("conv_0_weight_0"), Some(64));
    assert_eq!(at("conv_0_bias_0"), Some(4736));
    assert_eq!(at("conv_1_weight_0"), Some(4864));
    assert_eq!(at("conv_transpose_0_weight_0"), Some(197_184));
    assert_eq!(at("add_1_y_0"), Some(213_632));
    assert_eq!(at("conv_9_bias_0"), Some(244_992));
    assert_eq!(at("conv_11_weight_0"), Some(245_120));
    assert_eq!(at("conv_17_weight_0"), Some(829_696));
    assert_eq!(w.len(), 862_528);
}

#[test]
fn layer_one_weights_are_permuted_for_the_space_to_depth() {
    let blob = synth(BLOB_LEN, 1);
    let p = build_package(&blob, &synth(RESAMPLER_LEN, 2), Variant::Full).expect("builds");
    let w = p.file(package::WEIGHTS_PATH).expect("weights");
    let e = p
        .weights
        .iter()
        .find(|e| e.name == "conv_0_weight_0")
        .expect("entry");
    assert_eq!(e.len, 16 * 16 * 9 * 2);
    let src = |i: usize| u16::from_le_bytes([blob[2 * i], blob[2 * i + 1]]);
    let got = |i: usize| {
        let at = e.offset + 64 + 2 * i;
        u16::from_le_bytes([w[at], w[at + 1]])
    };
    for (co, q, dy, dx) in [
        (0, 0, 0, 0),
        (3, 5, 1, 2),
        (15, 15, 2, 2),
        (7, 1, 0, 1),
        (9, 6, 2, 0),
    ] {
        let k = 4 * (q % 4) + q / 4;
        let tap = 3 * dy + dx;
        assert_eq!(
            got(((co * 16 + q) * 3 + dy) * 3 + dx),
            src((tap * 16 + k) * 16 + co),
            "co {co} q {q} dy {dy} dx {dx}"
        );
    }
}

#[test]
fn dense_blocks_fold_both_stages() {
    // A block with W1_g = I (g = 0) and W2_0j = identity-like: the dense weight is then W[(m,j,n)][(r,c)] = [m == r] * W2[c][n].
    let mut res = vec![0u8; RESAMPLER_LEN];
    let put = |res: &mut Vec<u8>, byte: usize, i: usize, v: f32| {
        let at = byte + 2 * i;
        res[at..at + 2].copy_from_slice(&half::from_f32(v).to_le_bytes());
    };
    // block 1 at 0x800: W1_0 = identity: tiles [kb][rb] at +0, +0x80, +0x100, +0x180; diagonal in tiles (0,0) and (1,1).
    for d in 0..8 {
        put(&mut res, 0x800, d * 9, 1.0); // kb 0, rb 0
        put(&mut res, 0x800 + 0x180, d * 9, 1.0); // kb 1, rb 1
    }
    // W2_00 s = 0, element [i = 2][n = 3] = 0.5
    put(&mut res, 0x800 + 0x400, 2 * 8 + 3, 0.5);
    let blocks = super::resampler::blocks(&res).expect("blocks");
    let (w, cout) = super::resampler::dense(&blocks[1], false);
    assert_eq!(cout, 256);
    assert_eq!(w.len(), 256 * 256);
    let nonzero: Vec<usize> = (0..w.len()).filter(|&i| w[i] != 0).collect();
    // (m, j = 0, n = 3) for every m, input (r = m, c = 2): value 0.5
    let want: Vec<usize> = (0..16)
        .map(|m| ((m * 2) * 8 + 3) * 256 + m * 16 + 2)
        .collect();
    assert_eq!(nonzero, want);
    assert!(nonzero.iter().all(|&i| w[i] == half::from_f32(0.5)));
}

#[test]
fn the_same_weights_give_the_same_bytes() {
    let (a, b) = (full(), full());
    assert_eq!(a.files, b.files);
    let c = build_package(&synth(BLOB_LEN, 3), &synth(RESAMPLER_LEN, 2), Variant::Full)
        .expect("builds");
    assert_ne!(a.file(package::WEIGHTS_PATH), c.file(package::WEIGHTS_PATH));
    assert_eq!(
        a.file(package::MODEL_PATH).map(<[u8]>::len),
        c.file(package::MODEL_PATH).map(<[u8]>::len)
    );
}

#[test]
fn the_package_has_its_three_files() {
    let p = full();
    let paths: Vec<&str> = p.files.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        paths,
        [
            "Manifest.json",
            "Data/com.apple.CoreML/model.mlmodel",
            "Data/com.apple.CoreML/weights/weight.bin"
        ]
    );
    let manifest = String::from_utf8(p.files[0].1.clone()).expect("utf8");
    assert!(manifest.contains("\"rootModelIdentifier\": \"96188685-17FE-4E2C-9B1E-46D5BCF9CBC6\""));
    // The model starts with specificationVersion = 9 and holds the program (field 502).
    let model = p.file(package::MODEL_PATH).expect("model");
    assert_eq!(&model[..2], &[0x08, 0x09]);
}

#[test]
fn wrong_blob_lengths_are_errors() {
    assert!(build_package(&[0; 10], &synth(RESAMPLER_LEN, 2), Variant::Full).is_err());
    assert!(build_package(&synth(BLOB_LEN, 1), &[0; 10], Variant::Full).is_err());
    assert!(build_package(&synth(BLOB_LEN, 1), &[0; 10], Variant::UnetOnly).is_ok());
}

const COMPARE_PY: &str = r#"
import sys, numpy as np
sys.path.insert(0, sys.argv[1] + "/src")
import coremltools as ct
from build import build
d = sys.argv[2]
blob = open(d + "/blob.bin", "rb").read()
res = open(d + "/res.bin", "rb").read()
prog = build(blob, res, "fullnb")
ref = ct.convert(prog, convert_to="mlprogram", compute_precision=ct.precision.FLOAT16,
                 minimum_deployment_target=ct.target.macOS15, compute_units=ct.ComputeUnit.CPU_ONLY, skip_model_load=True)
ref.save(d + "/ref.mlpackage")
cpu = ct.ComputeUnit.CPU_ONLY
a = ct.models.MLModel(d + "/rust.mlpackage", compute_units=cpu)
b = ct.models.MLModel(d + "/ref.mlpackage", compute_units=cpu)
rng = np.random.default_rng(7)
x = rng.random((1, 4, 1080, 1920)).astype(np.float16)
x[:, 3] = 1.0
oa = a.predict({"x": x})
ob = b.predict({"x": x})
bad = False
for k in ("final", "delta"):
    da = np.abs(oa[k].astype(np.float32) - ob[k].astype(np.float32))
    print(k, oa[k].shape, "max abs diff", float(da.max()), "mean", float(da.mean()), "ref range", float(ob[k].min()), float(ob[k].max()))
    if oa[k].shape != ob[k].shape or da.max() > 0.02:
        bad = True
wa = open(d + "/rust.mlpackage/Data/com.apple.CoreML/weights/weight.bin", "rb").read()
wb = open(d + "/ref.mlpackage/Data/com.apple.CoreML/weights/weight.bin", "rb").read()
print("weight.bin sizes", len(wa), len(wb), "equal", wa == wb)
sys.exit(1 if bad else 0)
"#;

/// Loads the package with coremltools on the CPU and compares it with the prototype's model on synthetic weights.
/// `NXKIT_ANE_STUDY` is the study directory (`src/build.py` and a `.venv` with coremltools). Heavy: two 1080p CPU
/// predictions. Run: `NXKIT_ANE_STUDY=... nice -n 20 cargo test -p nxkit-ndve -- --ignored coremltools`.
#[test]
#[ignore = "needs python3 with coremltools (the study's venv) and runs a model on the CPU for a while"]
fn coremltools_loads_it_and_matches_the_prototype() {
    let Some(study) = std::env::var_os("NXKIT_ANE_STUDY") else {
        eprintln!("NXKIT_ANE_STUDY is not set, skipped");
        return;
    };
    let study = std::path::PathBuf::from(study);
    let py = study.join(".venv/bin/python");
    if !py.exists() {
        eprintln!("no venv in {}, skipped", study.display());
        return;
    }
    let dir = std::env::temp_dir().join(format!("nxkit-ane-test-{}", std::process::id()));
    let pkg = dir.join("rust.mlpackage");
    let blob = synth(BLOB_LEN, 11);
    let res = synth(RESAMPLER_LEN, 12);
    let p = build_package(&blob, &res, Variant::Full).expect("builds");
    for (path, bytes) in &p.files {
        let at = pkg.join(path);
        std::fs::create_dir_all(at.parent().expect("parent")).expect("mkdir");
        std::fs::write(at, bytes).expect("write");
    }
    std::fs::write(dir.join("blob.bin"), &blob).expect("write");
    std::fs::write(dir.join("res.bin"), &res).expect("write");
    std::fs::write(dir.join("compare.py"), COMPARE_PY).expect("write");
    let out = std::process::Command::new(&py)
        .arg(dir.join("compare.py"))
        .arg(&study)
        .arg(&dir)
        .current_dir(&study)
        .output()
        .expect("runs python");
    println!("{}", String::from_utf8_lossy(&out.stdout));
    eprintln!("{}", String::from_utf8_lossy(&out.stderr));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "the models differ or coremltools failed"
    );
}

/// Writes the package of synthetic weights to `$NXKIT_ANE_DUMP` (for looking at it with other tools).
#[test]
#[ignore = "a helper: set NXKIT_ANE_DUMP to a directory"]
fn dump_the_synthetic_package() {
    let Some(dir) = std::env::var_os("NXKIT_ANE_DUMP") else {
        return;
    };
    let p = full();
    for (path, bytes) in &p.files {
        let at = std::path::Path::new(&dir).join(path);
        std::fs::create_dir_all(at.parent().expect("parent")).expect("mkdir");
        std::fs::write(at, bytes).expect("write");
    }
}

#[test]
fn the_cache_key_names_the_weights_and_the_os() {
    let a = cache_key(&[1, 2, 3], "Version 26.0 (Build 25A1)");
    assert_eq!(a, cache_key(&[1, 2, 3], "Version 26.0 (Build 25A1)"));
    assert_ne!(a, cache_key(&[1, 2, 4], "Version 26.0 (Build 25A1)"));
    assert_ne!(a, cache_key(&[1, 2, 3], "Version 26.1 (Build 25B5)"));
    assert!(a.ends_with("-Version 26.0 (Build 25A1)"));
}
