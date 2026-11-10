//! Hand-built geometry programs through the MSL back end.
#![cfg(test)]

use super::{Context, Failure, Translated, translate};
use crate::job::{Job, parse_jobs};
use crate::translation::emit_test_util::{PRED7, RET};

/// `OUT.EMIT_THEN_CUT RZ, RZ, 0` and friends: the type is the two bits at 39.
fn out_i(ty: u64) -> u64 {
    0x3DB8u64 << 50 | 0xff | 0xff << 8 | PRED7 | ty << 39
}

pub(super) fn job(extra: &str) -> Job {
    let json = format!(r#"[{{"key": "gs0", "stage": "gs"{extra}}}]"#);
    parse_jobs(&json).expect("job").remove(0)
}

/// A Maxwell geometry program: the 0x50 byte header (one thread per primitive, triangle strip, 6 vertices) and the ops.
pub(super) fn code(word0: u32, ops: &[u64]) -> Vec<u8> {
    let mut h = [0u32; 20];
    h[0] = 4 << 10 | word0;
    h[2] = 1 << 24;
    h[3] = 7 << 24;
    h[4] = 6;
    let mut words: Vec<u64> = vec![0];
    for (i, w) in ops.iter().chain([RET].iter()).enumerate() {
        words.push(*w);
        if i % 3 == 2 {
            words.push(0);
        }
    }
    let mut c: Vec<u8> = h.iter().flat_map(|w| w.to_le_bytes()).collect();
    c.extend(words.iter().flat_map(|w| w.to_le_bytes()));
    c
}

pub(super) fn run(j: &Job, c: &[u8]) -> Result<Translated, Failure> {
    translate(j, c, &Context::default())
}

pub(super) fn extra<'a>(r: &'a super::Reflection, k: &str) -> Option<&'a nxb_pyjson::Value> {
    r.extra.iter().find(|(n, _)| n == k).map(|(_, v)| v)
}

fn emitting() -> Translated {
    run(&job(""), &code(0, &[out_i(3)])).expect("translate")
}

#[test]
fn a_geometry_shader_has_the_kernel_libraries() {
    let t = emitting();
    let names: Vec<&str> = t.libraries.iter().map(|l| l.suffix.as_str()).collect();
    assert_eq!(
        names,
        ["_b", "_bg0", "_bg1", "_bg2", "_bg3", "_bg4", "_bgv"]
    );
    for l in &t.libraries {
        assert!(l.reflection.is_some(), "{}", l.suffix);
    }
}

#[test]
fn a_kernel_is_a_compute_entry_with_the_helper_buffers() {
    let t = emitting();
    let l = &t.libraries[4];
    assert_eq!(l.suffix, "_bg3");
    assert!(l.msl.contains("kernel void main0"), "{}", l.msl);
    assert!(
        l.msl
            .contains("device geometry_vb_output_block* geometry_vb_output")
    );
    assert!(l.msl.contains("geometry_vb_output"));
    assert!(l.msl.contains("geometry_ib_output"));
    let r = l.reflection.as_ref().expect("reflection");
    assert_eq!(r.key, "gs0_bg3");
    assert_eq!(r.threadgroup_size, Some([32, 1, 1]));
    assert!(r.drv_unfillable.is_empty());
    let g = extra(r, "geometry").expect("geometry");
    assert_eq!(g.get("vertices").and_then(|v| v.as_i64()), Some(6));
    assert_eq!(g.get("output").and_then(|v| v.as_i64()), Some(7));
    assert_eq!(g.get("index_stride").and_then(|v| v.as_i64()), Some(12));
    assert!(g.get("stride").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    let slots: Vec<(&str, Option<i64>)> = r
        .bindings
        .iter()
        .filter(|b| b.kind.as_str() == "geometry")
        .map(|b| (b.msl_name.as_str(), b.reg))
        .collect();
    assert!(slots.contains(&("geometry_vb_output", Some(1))));
    assert!(slots.contains(&("geometry_ib_output", Some(2))));
}

#[test]
fn the_base_library_is_the_first_kernel() {
    let t = emitting();
    assert_eq!(t.libraries[0].msl, t.libraries[1].msl);
    let r = t.libraries[0].reflection.as_ref().expect("reflection");
    assert_eq!(r.key, "gs0_b");
}

#[test]
fn the_reader_draws_the_kernel_output() {
    let t = emitting();
    let l = t.libraries.last().expect("reader");
    assert_eq!(l.suffix, "_bgv");
    assert!(l.msl.contains("vertex "), "{}", l.msl);
    assert!(l.msl.contains("main0_z"), "{}", l.msl);
    assert!(l.msl.contains("vb_input"), "{}", l.msl);
    let r = l.reflection.as_ref().expect("reflection");
    assert_eq!(r.key, "gs0_bgv");
    assert_eq!(r.stage, "vs");
    assert_eq!(r.msl_entry_kind, "vertex");
    assert!(r.notes.is_empty());
    assert!(r.vertex_inputs.is_empty());
    assert!(r.entries.iter().any(|(k, _)| k == "entry_z"));
    assert!(extra(r, "geometry_reader").is_some());
    assert!(
        r.bindings
            .iter()
            .any(|b| b.msl_name == "vb_input" && b.reg == Some(1))
    );
}

#[test]
fn only_the_main_entry_converts_depth() {
    let t = emitting();
    let m = &t.libraries.last().expect("reader").msl;
    let (main, z) = m.split_once("main0_z").expect("two entries");
    assert!(main.contains("fma(out.gl_Position.z"), "{m}");
    assert!(!z.contains("fma(out.gl_Position.z"), "{m}");
}

#[test]
fn a_bad_header_fails_the_translation() {
    let mut c = code(0, &[out_i(3)]);
    c[12..16].copy_from_slice(&(2u32 << 24).to_le_bytes());
    let e = run(&job(""), &c).expect_err("topology");
    assert_eq!(e.reason, "translation failed");
    assert!(e.detail.contains("output declaration"), "{}", e.detail);
}

#[test]
fn tessellation_is_not_ported() {
    for stage in ["hs", "ds"] {
        let json = format!(r#"[{{"key": "t", "stage": "{stage}"}}]"#);
        let j = parse_jobs(&json).expect("job").remove(0);
        let e = run(&j, &[0; 64]).expect_err("tessellation");
        assert_eq!(e.reason, "not ported");
        assert!(e.detail.contains("tessellation"), "{}", e.detail);
    }
}

/// The kernels and the reader compile with Apple's compiler. Run by hand: `cargo test -p nxb-xlate -- --ignored metal`.
#[test]
#[ignore]
fn metal_compiles_the_geometry_libraries() {
    for l in &emitting().libraries {
        compile_metal(&format!("geom{}", l.suffix), &l.msl);
    }
}

/// Compile one source with Apple's compiler into `scratch/<name>.air`.
pub(super) fn compile_metal(name: &str, msl: &str) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scratch");
    std::fs::create_dir_all(&dir).expect("scratch");
    let src = dir.join(format!("{name}.metal"));
    let air = dir.join(format!("{name}.air"));
    std::fs::write(&src, msl).expect("write");
    let o = std::process::Command::new("xcrun")
        .args([
            "-sdk",
            "macosx",
            "metal",
            "-c",
            "-O1",
            "-fmetal-math-mode=relaxed",
            "-fpreserve-invariance",
            "-fwrapv",
            "-std=metal3.0",
        ])
        .arg(&src)
        .arg("-o")
        .arg(&air)
        .output()
        .expect("run metal");
    assert!(
        o.status.success(),
        "{name}: {}\n{msl}",
        String::from_utf8_lossy(&o.stderr)
    );
}

const TF: &str = r#", "tf": [{"stride": 16, "outputs": [[1, 4]]}, [], [], []]"#;

fn tf_reader() -> Translated {
    run(&job(TF), &code(0, &[out_i(3)])).expect("translate")
}

#[test]
fn only_the_reader_captures_a_geometry_job() {
    let t = tf_reader();
    for l in &t.libraries {
        assert_eq!(
            l.msl.contains("main0_tf"),
            l.suffix == "_bgv",
            "{}",
            l.suffix
        );
    }
}

#[test]
fn the_reader_capture_takes_its_vertex_from_the_order_buffer() {
    let t = tf_reader();
    let m = &t.libraries.last().expect("reader").msl;
    assert!(m.contains("vertex void main0_tf("), "{m}");
    assert!(m.contains("device uint* dm_tf0 [[buffer(8)]]"), "{m}");
    assert!(
        m.contains("constant dm_tf_capture& dm_tf [[buffer(14)]]"),
        "{m}"
    );
    assert!(m.contains("if (dm_linear < dm_tf.args.x)"), "{m}");
    assert!(
        m.contains("gl_VertexIndex = dm_tf.order[dm_linear];"),
        "{m}"
    );
    assert!(
        m.contains("dm_tf0[dm_tf.offset[0] / 4 + dm_linear * 4 + 0] = "),
        "{m}"
    );
}

#[test]
fn the_reader_reflection_has_the_capture_fields() {
    let t = tf_reader();
    let r = t
        .libraries
        .last()
        .expect("reader")
        .reflection
        .as_ref()
        .expect("reflection");
    let e = r
        .entries
        .iter()
        .find(|(k, _)| k == "entry_tf")
        .map(|(_, v)| v);
    assert_eq!(e.and_then(|v| v.as_str()), Some("main0_tf"));
    let s: Vec<i64> = extra(r, "tf_strides")
        .and_then(|v| v.as_list())
        .expect("strides")
        .iter()
        .filter_map(|v| v.as_i64())
        .collect();
    assert_eq!(s, [16, 0, 0, 0]);
    assert!(extra(r, "geometry_reader").is_some());
    for l in &t.libraries[..6] {
        let r = l.reflection.as_ref().expect("reflection");
        assert!(extra(r, "tf_strides").is_none(), "{}", l.suffix);
    }
}

#[test]
fn a_geometry_job_without_a_layout_has_no_capture() {
    let t = run(
        &job(r#", "tf": [null, null, null, null]"#),
        &code(0, &[out_i(3)]),
    )
    .expect("translate");
    assert!(t.libraries.iter().all(|l| !l.msl.contains("main0_tf")));
}

/// The reader with its capture entry compiles. Run by hand: `cargo test -p nxb-xlate -- --ignored metal`.
#[test]
#[ignore]
fn metal_compiles_the_geometry_capture() {
    let t = tf_reader();
    let l = t.libraries.last().expect("reader");
    compile_metal("geomtf", &l.msl);
}
