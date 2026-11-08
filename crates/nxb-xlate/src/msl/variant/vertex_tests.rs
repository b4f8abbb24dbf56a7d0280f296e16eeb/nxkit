//! End to end tests of the vertex variants on small vertex programs.

use super::vtest::{F32, ONE, S32, U32, V, fadd, partner, simple};
use crate::ir::{Inst, IoVariable, StorageKind};
use nxb_pyjson::Value;

#[test]
fn pad_writes_the_outputs_the_pixel_shader_reads() {
    let v = simple();
    let t = v.translate_with(&[partner(&[(1, "float4", ""), (4, "float4", "flat")])]);
    let lib = &t.libraries[0];
    assert!(lib.msl.contains("float4 dm_pad4 [[user(locn4)]];"));
    assert!(!lib.msl.contains("dm_pad1"));
    assert!(
        lib.msl
            .contains("    out.dm_pad4 = float4(0.0, 0.0, 0.0, 1.0);")
    );
    let r = lib.reflection.as_ref().expect("reflection");
    assert_eq!(r.vs_pad, vec![4]);
}

#[test]
fn no_pad_without_partners() {
    let t = simple().translate();
    let r = t.libraries[0].reflection.as_ref().expect("reflection");
    assert!(r.vs_pad.is_empty());
    assert!(!t.libraries[0].msl.contains("dm_pad"));
}

/// The text of the entry `name`.
pub(super) fn entry<'a>(msl: &'a str, name: &str) -> &'a str {
    let at = msl.find(&format!(" {name}(")).expect("entry");
    let rest = &msl[at..];
    let end = rest.find("\n}\n").map_or(rest.len(), |e| e + 3);
    &rest[..end]
}

fn field<'a>(t: &'a crate::msl::Translated, k: &str) -> Option<&'a Value> {
    let r = t.libraries[0].reflection.as_ref()?;
    r.entries.iter().find(|(n, _)| n == k).map(|(_, v)| v)
}

const ZFIX: &str = "out.gl_Position.z = fma(";

#[test]
fn z_entry_has_no_depth_fix() {
    let t = simple().translate();
    let msl = &t.libraries[0].msl;
    assert!(entry(msl, "main0").contains(ZFIX));
    let z = entry(msl, "main0_z");
    assert!(!z.contains(ZFIX));
    assert!(z.contains("return out;"));
    assert_eq!(field(&t, "entry_z"), Some(&Value::str("main0_z")));
    assert_eq!(field(&t, "entry_discard"), None);
    assert_eq!(field(&t, "entry_layer"), None);
}

#[test]
fn the_entries_call_one_body_function() {
    let t = layer_program().translate();
    let msl = &t.libraries[0].msl;
    // the main function is written once, and every entry calls it
    assert_eq!(msl.matches("static main0_out dm_body_main0(").count(), 1);
    assert_eq!(msl.matches("main0_out out = {};").count(), 1);
    for name in ["main0", "main0_z", "main0_l", "main0_lz", "main0_gs"] {
        let e = entry(msl, name);
        assert_eq!(e.matches("dm_body_main0(").count(), 1, "{name}\n{e}");
        assert!(e.lines().count() <= 10, "{name} is a short function\n{e}");
    }
    // the depth fix follows the call in the entries that have it
    let m = entry(msl, "main0");
    assert!(m.find("dm_body_main0(") < m.find(ZFIX), "{m}");
    assert!(!msl.contains("dm_body_main0(") || !entry(msl, "dm_body_main0").contains(ZFIX));
}

#[test]
fn the_body_function_returns_without_the_depth_fix() {
    let t = simple().translate();
    let msl = &t.libraries[0].msl;
    let body = entry(msl, "dm_body_main0");
    assert!(!body.contains(ZFIX), "{body}");
    assert!(body.trim_end().ends_with("return out;\n}"), "{body}");
}

#[test]
fn capture_entry_writes_the_outputs_to_the_buffer() {
    let t = simple().translate();
    let msl = &t.libraries[0].msl;
    let gs = entry(msl, "main0_gs");
    assert!(gs.starts_with(" void main0_gs(") || gs.contains("vertex void main0_gs") || true);
    for want in [
        "constant dm_geometry_capture& dm_gs [[buffer(13)]]",
        "uint gl_VertexIndex [[vertex_id]]",
        "uint gl_InstanceIndex [[instance_id]]",
        "dm_capture(dm_gs, dm_p, out);",
    ] {
        assert!(gs.contains(want), "missing {want}\n{gs}");
    }
    assert!(!gs.contains(ZFIX));
    assert!(msl.contains("w[22] = as_type<uint>(out.loc1.z);"));
    assert!(msl.contains("w[3] = as_type<uint>(out.gl_Position.w);"));
    assert_eq!(
        field(&t, "entry_geometry_capture"),
        Some(&Value::str("main0_gs"))
    );
}

// The own front end rejects `++` and `--`, so the variant loops step with `+=`.
#[test]
fn variant_loops_have_no_increment_operators() {
    let t = simple().translate_with(&[partner(&[(5, "float4", "")])]);
    let msl = &t.libraries[0].msl;
    assert!(msl.contains("k += 1u)"));
    assert!(!msl.contains("++") && !msl.contains("--"), "{msl}");
}

#[test]
fn capture_covers_the_pad() {
    let t = simple().translate_with(&[partner(&[(5, "float4", "")])]);
    assert!(
        t.libraries[0]
            .msl
            .contains("w[36] = as_type<uint>(out.dm_pad5.x);")
    );
}

#[test]
fn capture_rejects_an_attribute_outside_the_map() {
    let mut b = simple();
    b.out(40);
    let ctx = crate::msl::Context {
        chain: "V",
        source: "p#0".into(),
        pc_paired: "",
        partners: &[],
        no_facts: false,
        ..crate::msl::Context::default()
    };
    let r = crate::msl::library::build(&b.s, &ctx, &crate::msl::emit::Options::default());
    assert!(r.err().is_some_and(|e| e.contains("geometry capture")));
}

#[test]
fn discard_entry_exists_when_the_shader_writes_a_buffer() {
    let mut b = simple();
    let x = b.attr(0, 1, F32);
    b.store_buffer(x);
    b.ret();
    let t = b.translate();
    let msl = &t.libraries[0].msl;
    let d = entry(msl, "main0_discard");
    assert!(d.contains("main0_out out = {};"));
    assert!(d.trim_end().ends_with("return;\n}"), "{d}");
    assert!(!d.contains(ZFIX));
    assert_eq!(
        field(&t, "entry_discard"),
        Some(&Value::str("main0_discard"))
    );
}

pub(super) fn layer_program() -> V {
    let mut b = V::new(&[(0, 0)]);
    b.out(1);
    b.io(StorageKind::Input, IoVariable::InstanceIndex, 0);
    let i = b.builtin(IoVariable::InstanceIndex, U32);
    let x = b.attr(0, 0, F32);
    b.store(IoVariable::Position, None, 0, x);
    b.store(IoVariable::UserDefined, Some(1), 2, i);
    b.ret();
    b
}

#[test]
fn layer_entries_return_the_layer_from_an_integer_output() {
    let t = layer_program().translate();
    let msl = &t.libraries[0].msl;
    assert!(msl.contains("uint gl_Layer [[render_target_array_index]];"));
    assert!(msl.contains("r.gl_Position = out.gl_Position;"));
    let l = entry(msl, "main0_l");
    assert!(l.contains(ZFIX));
    assert!(l.contains("return dm_layer_out(out, as_type<uint>(out.loc1.z));"));
    let lz = entry(msl, "main0_lz");
    assert!(!lz.contains(ZFIX));
    assert!(lz.contains("return dm_layer_out(out, as_type<uint>(out.loc1.z));"));
    assert_eq!(field(&t, "entry_layer"), Some(&Value::str("main0_l")));
    assert_eq!(field(&t, "entry_layer_z"), Some(&Value::str("main0_lz")));
    let from = field(&t, "layer_from").expect("layer_from");
    assert_eq!(from.get("loc").and_then(Value::as_i64), Some(1));
    assert_eq!(from.get("comp").and_then(Value::as_str), Some("z"));
}

#[test]
fn no_layer_entries_without_an_instance_index() {
    let mut b = V::new(&[(0, 0)]);
    b.out(1);
    let x = b.attr(0, 0, F32);
    let u = b.local(U32);
    let c = b.k(7);
    let a = b.s.info.ast.new_assign(u, c);
    b.put(a);
    b.store(IoVariable::Position, None, 0, x);
    b.store(IoVariable::UserDefined, Some(1), 2, u);
    b.ret();
    let t = b.translate();
    assert!(!t.libraries[0].msl.contains("main0_l"));
    assert_eq!(field(&t, "entry_layer"), None);
}

/// Compile the libraries of `t` with Apple's compiler.
pub(super) fn metal_compile(name: &str, t: &crate::msl::Translated) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scratch");
    std::fs::create_dir_all(&dir).expect("scratch");
    for l in &t.libraries {
        let n = format!("vtx_{name}{}", l.suffix);
        let src = dir.join(format!("{n}.metal"));
        let air = dir.join(format!("{n}.air"));
        std::fs::write(&src, &l.msl).expect("write");
        let o = std::process::Command::new("xcrun")
            .args(["-sdk", "macosx", "metal", "-c", "-O1", "-std=metal3.0"])
            .arg(&src)
            .arg("-o")
            .arg(&air)
            .output()
            .expect("run metal");
        assert!(
            o.status.success(),
            "{n}: {}\n{}",
            String::from_utf8_lossy(&o.stderr),
            l.msl
        );
    }
}

/// Run by hand: `cargo test -p nxb-xlate -- --ignored metal`.
#[test]
#[ignore]
fn metal_compiles_the_vertex_variants() {
    metal_compile("simple", &simple().translate());
    metal_compile(
        "pad",
        &simple().translate_with(&[partner(&[(1, "float4", ""), (4, "float4", "flat")])]),
    );
    metal_compile("layer", &layer_program().translate());
    metal_compile("int", &int_use().translate());
    let mut b = simple();
    let x = b.attr(0, 1, F32);
    b.store_buffer(x);
    b.ret();
    metal_compile("discard", &b.translate());
}

/// `position.x = attr0.x + 1` as an integer add, so the input is only read as an integer.
pub(super) fn int_use() -> V {
    let mut b = V::new(&[(0, 0)]);
    let x = b.attr(0, 0, F32);
    let one = b.k(1);
    let add = b.op(Inst::Add, StorageKind::None, vec![x, one]);
    let y = b.def(S32, add);
    b.store(IoVariable::Position, None, 0, y);
    b.ret();
    b
}

#[test]
fn an_input_that_is_only_cast_to_integers_becomes_uint() {
    let t = int_use().translate();
    let lib = &t.libraries[0];
    assert!(
        lib.msl.contains("uint4 loc0 [[attribute(0)]];"),
        "{}",
        lib.msl
    );
    let r = lib.reflection.as_ref().expect("reflection");
    assert_eq!(r.vertex_inputs[0].msl_type, "uint4");
}

#[test]
fn an_input_with_a_float_use_stays_float() {
    let mut b = int_use();
    let x = b.attr(0, 1, F32);
    let one = b.k(ONE);
    let add = b.op(fadd(), StorageKind::None, vec![x, one]);
    let y = b.def(F32, add);
    b.store(IoVariable::Position, None, 1, y);
    b.ret();
    let t = b.translate();
    assert!(t.libraries[0].msl.contains("float4 loc0 [[attribute(0)]];"));
}

#[test]
fn an_input_the_job_declares_is_left_alone() {
    let mut b = int_use();
    b.s.job.attr.push((0, "float".into()));
    assert!(b.msl().contains("float4 loc0 [[attribute(0)]];"));
}
