//! The lean geometry kernels of passthrough geometry shaders.
#![cfg(test)]

use super::Translated;
use super::geom_lean::lean;
use super::geom_tests::{code, compile_metal, extra, job, run};
use crate::front::emit_test_util::PRED7;
use nxb_pyjson::Value;

/// `ALD R0, a[0x80]` of the vertex in RZ: generic attribute 0.x, capture word 16.
const ALD: u64 = 0xEFD8u64 << 48 | 0x080 << 20 | 0xff << 8 | 0xff << 39 | PRED7;
/// `AST a[0x3a0], R0`: the viewport mask.
const AST_MASK: u64 = 0xEFF0u64 << 48 | 0x3a0 << 20 | 0xff << 8 | 0xff << 39 | PRED7;
/// `OUT.EMIT_THEN_CUT RZ, RZ, 0`.
const OUT: u64 = 0x3DB8u64 << 50 | 0xff | 0xff << 8 | PRED7 | 3 << 39;

fn passthrough() -> Translated {
    run(&job(""), &code(1 << 24, &[ALD, AST_MASK])).expect("translate")
}

fn suffixes(t: &Translated) -> Vec<&str> {
    t.libraries.iter().map(|l| l.suffix.as_str()).collect()
}

fn field<'a>(g: &'a Value, k: &str) -> Option<&'a Value> {
    match g {
        Value::Object(f) => f.iter().find(|(n, _)| n == k).map(|(_, v)| v),
        _ => None,
    }
}

#[test]
fn a_passthrough_shader_has_the_lean_kernels() {
    let t = passthrough();
    assert_eq!(
        suffixes(&t),
        [
            "_b", "_bg0", "_bg1", "_bg2", "_bg3", "_bg4", "_bgv", "_bl0", "_bl1", "_bl3"
        ]
    );
    for l in &t.libraries {
        let r = l.reflection.as_ref().expect("reflection");
        assert!(
            r.notes.iter().all(|n| !n.contains("lean")),
            "{}: {:?}",
            l.suffix,
            r.notes
        );
    }
}

#[test]
fn a_shader_that_emits_has_no_lean_kernel() {
    let t = run(&job(""), &code(0, &[OUT])).expect("translate");
    assert!(suffixes(&t).iter().all(|s| !s.starts_with("_bl")));
}

#[test]
fn the_lean_reflection_has_the_lean_layout() {
    let t = passthrough();
    for (s, full) in [("_bl0", "_bg0"), ("_bl1", "_bg1"), ("_bl3", "_bg3")] {
        let lib = |x: &str| t.libraries.iter().find(|l| l.suffix == x).expect(x);
        let r = lib(s).reflection.as_ref().expect("reflection");
        let f = lib(full).reflection.as_ref().expect("reflection");
        assert_eq!(r.key, format!("gs0{s}"));
        let g = extra(r, "geometry").expect("geometry");
        assert_eq!(field(g, "stride"), Some(&Value::Int(1)));
        assert_eq!(field(g, "lean"), Some(&Value::Bool(true)));
        assert_eq!(
            field(g, "capture_reads"),
            Some(&Value::List(vec![Value::Int(16)]))
        );
        let fg = extra(f, "geometry").expect("geometry");
        for k in ["vertices", "invocations", "output", "index_stride"] {
            assert_eq!(field(g, k), field(fg, k), "{s} {k}");
        }
        assert!(field(fg, "lean").is_none());
        assert!(r.bindings.iter().all(|b| !b.msl_type.contains("volatile")));
    }
}

#[test]
fn the_lean_kernel_writes_only_the_mask_and_the_indices() {
    let t = passthrough();
    let full = t
        .libraries
        .iter()
        .find(|l| l.suffix == "_bg3")
        .expect("_bg3");
    let l = t
        .libraries
        .iter()
        .find(|l| l.suffix == "_bl3")
        .expect("_bl3");
    let stores = |m: &str| m.matches("geometry_vb_output)._m0[").count();
    // One mask store for each of the three vertices of a triangle.
    assert!(stores(&full.msl) > 3);
    assert_eq!(stores(&l.msl), 3);
    assert_eq!(l.msl.matches(" / 134);").count(), 3);
    assert!(l.msl.contains("geometry_ib_output)._m0["));
    assert!(!l.msl.contains("volatile"));
    assert!(!l.msl.contains(" * 144);"));
    assert!(l.msl.contains(" * 1);"));
    assert!(l.msl.len() < full.msl.len());
}

/// A full kernel: one capture read of word 16 feeds the viewport mask of output vertex `v9`.
fn kernel(body: &str) -> String {
    format!(
        "struct geometry_vb_output_block {{\n    float _m0[1];\n}};\n\
         struct spvDescriptorSetBuffer0\n{{\n\
         \x20   const device volatile vertex_output_block* vertex_output [[id(1)]];\n\
         \x20   device geometry_vb_output_block* geometry_vb_output [[id(2)]];\n}};\n\
         kernel void main0(constant spvDescriptorSetBuffer0& spvDescriptorSet0 [[buffer(0)]], uint3 id [[thread_position_in_grid]])\n\
         {{\n    int v0;\n    int v1;\n    int v2;\n    float v3;\n    int v4;\n    int v5;\n    int v6;\n    int v9;\n    int v12;\n\
         \x20   v0 = int(id.x);\n    v9 = int(id.y);\n    v1 = (v0 * 144);\n    v2 = (v1 + 16);\n\
         \x20   v3 = (*spvDescriptorSet0.vertex_output)._m0[v2];\n    v4 = (v9 * 134);\n    v5 = (v4 + 5);\n    v6 = (v4 + 0);\n\
         {body}}}\n"
    )
}

const MASK: &str = "    (*spvDescriptorSet0.geometry_vb_output)._m0[v5] = v3;\n";
const POS: &str = "    (*spvDescriptorSet0.geometry_vb_output)._m0[v6] = v3;\n";

#[test]
fn lean_keeps_the_mask_store_and_renumbers_the_record() {
    let l = lean(&kernel(&format!("{POS}{MASK}")), 134).expect("lean");
    assert_eq!(l.reads, [16]);
    assert!(l.msl.contains("    v1 = (v0 * 1);\n    v2 = (v1 + 0);\n"));
    assert!(l.msl.contains("    v5 = (v4 / 134);\n"));
    assert!(!l.msl.contains("v6"));
    assert!(
        l.msl
            .contains("const device vertex_output_block* vertex_output")
    );
}

#[test]
fn lean_fails_without_a_kernel_entry() {
    assert_eq!(
        lean("struct x {};\n", 134),
        Err("lean geometry: no kernel entry".into())
    );
}

#[test]
fn lean_fails_without_a_mask_store() {
    assert_eq!(
        lean(&kernel(POS), 134),
        Err("lean geometry: no viewport mask store".into())
    );
}

#[test]
fn lean_fails_on_a_computed_output_store() {
    let body = format!("{MASK}    (*spvDescriptorSet0.geometry_vb_output)._m0[(v4 + v0)] = v3;\n");
    assert_eq!(
        lean(&kernel(&body), 134),
        Err("lean geometry: unknown output store".into())
    );
}

#[test]
fn lean_fails_when_the_mask_index_has_other_readers() {
    let body = format!("{MASK}    if (v5 > 0) {{\n    }}\n");
    assert_eq!(
        lean(&kernel(&body), 134),
        Err("lean geometry: unknown output store".into())
    );
}

#[test]
fn lean_fails_on_a_computed_capture_read() {
    let src = kernel(MASK).replace("v2 = (v1 + 16);", "v2 = (v1 + v9);");
    assert_eq!(
        lean(&src, 134),
        Err("lean geometry: computed capture read".into())
    );
}

#[test]
fn lean_fails_on_an_unknown_capture_read() {
    let src = kernel(MASK).replace("._m0[v2];", "._m0[v2] + 1.0f;");
    assert_eq!(
        lean(&src, 134),
        Err("lean geometry: unknown capture read".into())
    );
}

#[test]
fn lean_fails_when_the_record_base_has_other_readers() {
    let body = format!("{MASK}    if (v1 > 0) {{\n    }}\n");
    assert_eq!(
        lean(&kernel(&body), 134),
        Err("lean geometry: record base used elsewhere".into())
    );
}

#[test]
fn lean_removes_dead_local_array_elements() {
    let body =
        format!("    float v7[2];\n    v7[0] = v3;\n    v7[1] = v3;\n    v3 = v7[1];\n{MASK}");
    let l = lean(&kernel(&body), 134).expect("lean");
    assert!(!l.msl.contains("v7[0]"));
    assert!(l.msl.contains("v7[1] = v3;"));
}

/// The lean kernels compile with Apple's compiler. Run by hand: `cargo test -p nxb-xlate -- --ignored metal`.
#[test]
#[ignore]
fn metal_compiles_the_lean_kernels() {
    for l in passthrough()
        .libraries
        .iter()
        .filter(|l| l.suffix.starts_with("_bl"))
    {
        compile_metal(&format!("geom{}", l.suffix), &l.msl);
    }
}
