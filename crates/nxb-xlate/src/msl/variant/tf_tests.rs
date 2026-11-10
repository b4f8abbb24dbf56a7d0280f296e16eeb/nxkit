//! The transform feedback capture entry on small vertex programs.

use super::vertex_tests::entry;
use super::vtest::{V, simple};
use crate::job::{TfBuffer, TransformFeedback};
use crate::msl::Translated;
use nxb_pyjson::Value;

/// Buffer 0 takes outputs 1 (four components) and 5 (two), buffer 2 takes output 3 (one).
fn layout() -> TransformFeedback {
    let mut tf = TransformFeedback::default();
    tf.buffers[0] = Some(TfBuffer {
        stride: 24,
        outputs: vec![(1, 4), (5, 2)],
    });
    tf.buffers[2] = Some(TfBuffer {
        stride: 4,
        outputs: vec![(3, 1)],
    });
    tf
}

fn with_tf() -> V {
    let mut v = simple();
    v.s.job.tf = Some(layout());
    v
}

fn field<'a>(t: &'a Translated, k: &str) -> Option<&'a Value> {
    let r = t.libraries[0].reflection.as_ref()?;
    r.entries
        .iter()
        .chain(r.extra.iter())
        .find(|(n, _)| n == k)
        .map(|(_, v)| v)
}

#[test]
fn the_capture_entry_has_the_feedback_buffers_and_the_state() {
    let t = with_tf().translate();
    let msl = &t.libraries[0].msl;
    assert!(msl.contains("struct dm_tf_capture\n"), "{msl}");
    let e = entry(msl, "main0_tf");
    assert!(e.starts_with(" main0_tf("), "{e}");
    assert!(msl.contains("vertex void main0_tf("), "{msl}");
    assert!(e.contains("device uint* dm_tf0 [[buffer(8)]]"), "{e}");
    assert!(!e.contains("dm_tf1"), "{e}");
    assert!(e.contains("device uint* dm_tf2 [[buffer(10)]]"), "{e}");
    assert!(
        e.contains("constant dm_tf_capture& dm_tf [[buffer(14)]]"),
        "{e}"
    );
    assert!(e.contains("uint dm_vid [[vertex_id]]"), "{e}");
    assert!(e.contains("uint dm_iid [[instance_id]]"), "{e}");
}

#[test]
fn the_capture_guards_and_stores_one_record_per_vertex() {
    let t = with_tf().translate();
    let e = entry(&t.libraries[0].msl, "main0_tf").to_string();
    assert!(
        e.contains(
            "uint dm_linear = (dm_iid - dm_tf.base_instance) * dm_tf.count + dm_vid - dm_tf.first;"
        ),
        "{e}"
    );
    assert!(!e.contains("dm_tf.order"), "{e}");
    assert!(e.contains("if (dm_linear < dm_tf.args.x)"), "{e}");
    assert!(!e.contains("return out;"), "{e}");
    // Output 1 is written by the shader. Its x component is not, but the member holds all four.
    for (i, c) in ["x", "y", "z", "w"].iter().enumerate() {
        let line = format!(
            "dm_tf0[dm_tf.offset[0] / 4 + dm_linear * 6 + {i}] = as_type<uint>(out.loc1.{c});"
        );
        assert!(e.contains(&line), "{line}\n{e}");
    }
    // Output 5 is not in the shader: zero for x, one for w, and only two components are captured.
    assert!(
        e.contains("dm_tf0[dm_tf.offset[0] / 4 + dm_linear * 6 + 4] = 0u;"),
        "{e}"
    );
    assert!(
        e.contains("dm_tf0[dm_tf.offset[0] / 4 + dm_linear * 6 + 5] = 0u;"),
        "{e}"
    );
    assert!(
        e.contains("dm_tf2[dm_tf.offset[2] / 4 + dm_linear * 1 + 0] = 0u;"),
        "{e}"
    );
    assert!(!e.contains("+ 6]"), "{e}");
}

#[test]
fn a_missing_fourth_component_is_one() {
    let mut v = simple();
    let mut tf = TransformFeedback::default();
    tf.buffers[3] = Some(TfBuffer {
        stride: 16,
        outputs: vec![(7, 4)],
    });
    v.s.job.tf = Some(tf);
    let t = v.translate();
    let e = entry(&t.libraries[0].msl, "main0_tf");
    assert!(e.contains("device uint* dm_tf3 [[buffer(11)]]"), "{e}");
    assert!(e.contains("dm_linear * 4 + 3] = 0x3f800000u;"), "{e}");
    assert!(e.contains("dm_linear * 4 + 2] = 0u;"), "{e}");
}

#[test]
fn a_pad_of_the_pixel_shader_is_captured_as_a_vector() {
    use super::vtest::partner;
    let mut v = simple();
    let mut tf = TransformFeedback::default();
    tf.buffers[1] = Some(TfBuffer {
        stride: 16,
        outputs: vec![(4, 4)],
    });
    v.s.job.tf = Some(tf);
    let t = v.translate_with(&[partner(&[(4, "float4", "flat")])]);
    let e = entry(&t.libraries[0].msl, "main0_tf");
    assert!(e.contains("as_type<uint>(out.dm_pad4.x)"), "{e}");
}

#[test]
fn the_main_entries_are_unchanged() {
    let plain = simple().translate().libraries[0].msl.clone();
    let t = with_tf().translate();
    let msl = &t.libraries[0].msl;
    for name in ["main0", "main0_z"] {
        assert_eq!(entry(msl, name), entry(&plain, name), "{name}");
    }
}

#[test]
fn the_reflection_names_the_entry_and_the_strides() {
    let t = with_tf().translate();
    assert_eq!(
        field(&t, "entry_tf").and_then(Value::as_str),
        Some("main0_tf")
    );
    let strides: Vec<i64> = field(&t, "tf_strides")
        .and_then(Value::as_list)
        .expect("strides")
        .iter()
        .filter_map(Value::as_i64)
        .collect();
    assert_eq!(strides, [24, 0, 4, 0]);
}

#[test]
fn no_layout_no_capture() {
    let t = simple().translate();
    assert!(!t.libraries[0].msl.contains("main0_tf"));
    assert!(field(&t, "entry_tf").is_none());
    assert!(field(&t, "tf_strides").is_none());
}

#[test]
fn only_the_base_library_captures() {
    // An integer attribute makes the plain and the vertex-fetch variants appear.
    let mut v = V::new(&[(0, 0), (1, 1)]);
    v.out(1);
    let x = v.attr(0, 0, super::vtest::F32);
    v.store(crate::ir::IoVariable::Position, None, 0, x);
    v.store(crate::ir::IoVariable::UserDefined, Some(1), 0, x);
    v.ret();
    v.s.job.tf = Some(layout());
    let t = v.translate();
    let names: Vec<&str> = t.libraries.iter().map(|l| l.suffix.as_str()).collect();
    assert_eq!(names[0], "_b");
    assert!(names.len() > 1, "{names:?}");
    assert!(t.libraries[0].msl.contains("main0_tf"));
    for l in &t.libraries[1..] {
        assert!(!l.msl.contains("main0_tf"), "{}", l.suffix);
        assert!(!l.msl.contains("dm_tf"), "{}", l.suffix);
        let Some(r) = l.reflection.as_ref() else {
            continue;
        };
        assert!(
            r.entries.iter().all(|(k, _)| k != "entry_tf"),
            "{}",
            l.suffix
        );
        assert!(
            r.extra.iter().all(|(k, _)| k != "tf_strides"),
            "{}",
            l.suffix
        );
    }
}

#[test]
fn definitions_with_transform_feedback_are_refused() {
    let mut v = simple();
    v.s.props.definitions.transform_feedback_enabled = true;
    let ctx = crate::msl::Context::default();
    let e = crate::msl::library::build(&v.s, &ctx, &crate::msl::emit::Options::default())
        .expect_err("refused");
    assert!(e.starts_with("unported: transform feedback"), "{e}");
}

/// The capture entry compiles with Apple's compiler. Run by hand: `cargo test -p nxb-xlate -- --ignored metal`.
#[test]
#[ignore]
fn metal_compiles_the_capture_entry() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scratch");
    std::fs::create_dir_all(&dir).expect("scratch");
    let t = with_tf().translate();
    let src = dir.join("tf.metal");
    let air = dir.join("tf.air");
    std::fs::write(&src, &t.libraries[0].msl).expect("write");
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
        "{}\n{}",
        String::from_utf8_lossy(&o.stderr),
        t.libraries[0].msl
    );
}
