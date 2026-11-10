//! Lowering of `mulhi`, the simd builtins, `front_facing` and `calculate_*_lod` on synthetic MSL.
//! The expected lines are what `metal -c -O1 -fmetal-math-mode=relaxed -fpreserve-invariance -fwrapv` emits for the same source.

use air_emit::{compile_source, Target};

const PRE: &str = "#include <metal_stdlib>\nusing namespace metal;\n";

fn emit(body: &str) -> String {
    compile_source(&format!("{PRE}{body}"), &Target::macos(), "t.metal").expect("emits")
}

fn has(ir: &str, s: &str) {
    assert!(ir.contains(s), "missing `{s}` in\n{ir}");
}

#[test]
fn mulhi_signed_and_unsigned() {
    let ir = emit(
        "kernel void k(device uint* p [[buffer(0)]], device int* q [[buffer(1)]], device int4* v [[buffer(2)]])\n\
         { p[0] = mulhi(p[1], 613566757u); q[0] = mulhi(q[1], -7); v[0] = mulhi(v[1], v[2]); }\n",
    );
    has(&ir, "call i32 @air.mul_hi.u.i32(i32 ");
    has(&ir, ", i32 613566757) #3");
    has(&ir, "call i32 @air.mul_hi.s.i32(i32 ");
    has(&ir, "call <4 x i32> @air.mul_hi.s.v4i32(<4 x i32> ");
    has(&ir, "declare i32 @air.mul_hi.u.i32(i32, i32) #3");
    has(&ir, "attributes #3 = { nounwind readnone willreturn }");
}

#[test]
fn simd_shuffle_freezes_the_data_and_votes_are_convergent() {
    let ir = emit(
        "struct O { float4 c0 [[color(0)]]; };\nstruct I { float4 u [[user(locn0)]]; };\n\
         fragment O main0(I in [[stage_in]], uint l [[thread_index_in_simdgroup]])\n\
         {\n    O o = {};\n    ushort n = ushort(l ^ 1u);\n\
             float f = simd_shuffle(in.u.x, n);\n    int i = simd_shuffle(as_type<int>(in.u.y), n);\n\
             uint u = simd_shuffle(as_type<uint>(in.u.z), n);\n    float2 v = simd_shuffle(in.u.zw, n);\n\
             bool b = simd_any(in.u.x > 0.0) && simd_all(in.u.y > 0.0);\n\
             o.c0 = float4(f + v.x, float(i), float(u), b ? 1.0 : 0.0);\n    return o;\n}\n",
    );
    has(
        &ir,
        "define <{ <4 x float> }> @main0(<4 x float> %a0, i32 noundef %a1)",
    );
    has(
        &ir,
        "!\"air.thread_index_in_simdgroup\", !\"air.arg_type_name\", !\"uint\"",
    );
    for (ty, f) in [
        (
            "float",
            "nsz arcp afn float @air.simd_shuffle.f32(float",
        ),
        ("i32", "i32 @air.simd_shuffle.s.i32(i32"),
        ("i32", "i32 @air.simd_shuffle.u.i32(i32"),
        (
            "<2 x float>",
            "nsz arcp afn <2 x float> @air.simd_shuffle.v2f32(<2 x float>",
        ),
    ] {
        // the data operand is a frozen value, the lane an i16
        let call = ir
            .lines()
            .find(|l| l.contains(f))
            .unwrap_or_else(|| panic!("missing {f} in\n{ir}"));
        let data = call[call.find(f).unwrap_or(0) + f.len()..]
            .split(',')
            .next()
            .unwrap_or("")
            .trim();
        has(&ir, &format!("{data} = freeze {ty} "));
        assert!(call.contains(", i16 ") && call.ends_with(") #12"), "{call}");
    }
    has(&ir, "call i1 @air.simd_any(i1 ");
    has(&ir, "call i1 @air.simd_all(i1 ");
    has(&ir, "declare i1 @air.simd_any(i1) #12");
    has(&ir, "attributes #12 = { convergent nounwind willreturn }");
    has(
        &ir,
        "attributes #11 = { convergent nobuiltin \"no-builtins\" }",
    );
    assert_eq!(ir.matches("attributes #11 ").count(), 1);
}

#[test]
fn front_facing_stage_in_field() {
    let ir = emit(
        "struct I { float4 p [[position]]; float4 u [[user(locn5)]]; bool ff [[front_facing]]; };\n\
         struct O { float4 c0 [[color(0)]]; };\n\
         fragment O main0(I in [[stage_in]]) { O o = {}; o.c0 = in.u + as_type<float>(in.ff ? -1 : 0); return o; }\n",
    );
    has(
        &ir,
        "define <{ <4 x float> }> @main0(<4 x float> %a0, <4 x float> %a1, i1 %a2)",
    );
    has(&ir, "!{i32 2, !\"air.front_facing\", !\"air.arg_type_name\", !\"bool\", !\"air.arg_name\", !\"ff\"}");
}

#[test]
fn calculate_lod_on_a_texture_array() {
    let ir = emit(
        "struct spvDescriptorSetBuffer0 { texture2d_array<float> t [[id(0)]]; sampler s [[id(1)]]; };\n\
         struct O { float4 c0 [[color(0)]]; };\nstruct I { float4 u [[user(locn0)]]; };\n\
         fragment O main0(I in [[stage_in]], constant spvDescriptorSetBuffer0& d [[buffer(0)]])\n\
         {\n    O o = {};\n    float l = d.t.calculate_clamped_lod(d.s, in.u.xy);\n\
             float m = d.t.calculate_unclamped_lod(d.s, in.u.zw);\n    o.c0 = float4(l, m, 0.0, 1.0);\n    return o;\n}\n",
    );
    has(&ir, "declare float @air.calculate_clamped_lod_texture_2d_array(%struct._texture_2d_array_t addrspace(1)* nocapture readonly, %struct._sampler_t addrspace(2)* nocapture readonly, <2 x float>, i32) #5");
    has(
        &ir,
        "declare float @air.calculate_unclamped_lod_texture_2d_array(",
    );
    has(
        &ir,
        "attributes #5 = { argmemonly convergent nounwind readonly willreturn }",
    );
}

#[test]
fn simd_builtins_need_the_right_types() {
    let e = compile_source(
        &format!("{PRE}kernel void k(device uint* p [[buffer(0)]]) {{ p[0] = simd_any(p[1]) ? 1u : 0u; }}\n"),
        &Target::macos(),
        "t",
    )
    .unwrap_err();
    assert!(e.key().contains("simd-vote-form"), "{}", e.key());
}
