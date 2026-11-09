//! Emission tests on synthetic MSL (no game data). The full check with Apple's tools is `airc check`.

use air_emit::{compile_source, Target};

const PRE: &str = "#include <metal_stdlib>\nusing namespace metal;\n";

fn emit(body: &str) -> String {
    compile_source(&format!("{PRE}{body}"), &Target::macos(), "t.metal").expect("emits")
}

#[test]
fn kernel_with_argument_buffer() {
    let ir = emit(
        "struct B { uint v[4]; };\nstruct spvDescriptorSetBuffer0 { device B* b [[id(0)]]; };\n\
         kernel void main0(constant spvDescriptorSetBuffer0& s [[buffer(0)]], uint3 g [[thread_position_in_grid]])\n\
         { (*s.b).v[g.x & 3u] = g.y << 2u; }\n",
    );
    assert!(ir.contains("define void @main0(%struct.spvDescriptorSetBuffer0 addrspace(2)* noundef align 8 dereferenceable(8) \"air-buffer-no-alias\" %a0, <3 x i32> noundef %a1)"));
    assert!(ir.contains("!\"air.indirect_buffer\", !\"air.buffer_size\", i32 8, !\"air.location_index\", i32 0, i32 1, !\"air.read\""));
    assert!(ir.contains("!\"air.thread_position_in_grid\""));
    assert!(ir.contains("shl i32"));
}

#[test]
fn fragment_with_function_constant_gate_and_contraction() {
    let ir = emit(
        "constant bool FB [[function_constant(3)]];\n\
         struct O { float4 c [[color(0), function_constant(FB)]]; };\n\
         struct I { float4 u [[user(locn0)]]; };\n\
         fragment O main0(I in [[stage_in]]) { O o; o.c = in.u * in.u + in.u; return o; }\n",
    );
    assert!(ir.contains("@_Z2FB.MTL_FC_INIT_3_b = linkonce_odr hidden addrspace(2) externally_initialized constant i8 undef"));
    assert!(ir.contains("@__metal_implicit_fc_pred_0"));
    assert!(ir.contains("@llvm.fmuladd.v4f32"));
    assert!(ir.contains(
        "!\"air.fragment_input\", !\"user(locn0)\", !\"air.center\", !\"air.perspective\""
    ));
    assert!(ir.contains("define <{ <4 x float> }> @main0(<4 x float> %a0)"));
}

#[test]
fn unsupported_constructs_are_reported() {
    let e = compile_source(
        &format!("{PRE}kernel void k(device float* p [[buffer(0)]]) {{ p[0] = tan(p[1]); }}\n"),
        &Target::macos(),
        "t",
    )
    .unwrap_err();
    assert!(
        e.key().starts_with("unsupported:builtin:tan"),
        "{}",
        e.key()
    );
}

#[test]
fn ios_convention() {
    let src = format!(
        "{PRE}struct spvDescriptorSetBuffer0 {{ texture2d<float> t [[id(0)]]; }};\n\
         fragment float4 main0(float4 p [[position]], constant spvDescriptorSetBuffer0& s [[buffer(0)]]) \
         {{ float4 v = s.t.read(uint2(p.xy)); return v * p + p; }}\n"
    );
    let ios = compile_source(&src, &Target::ios(), "-").expect("emits");
    assert!(ios.contains("target triple = \"air64_v27-apple-ios18.0.0\""));
    assert!(ios.contains("!{!\"Metal\", i32 3, i32 2, i32 0}"));
    assert!(!ios.contains("nnan") && !ios.contains("no-nans-fp-math"));
    assert!(!ios.contains("air.get_read_sampler"));
    assert!(ios.contains("!{!\"omnipotent char\""));
    let mac = compile_source(&src, &Target::macos(), "-").expect("emits");
    assert!(mac.contains("air.get_read_sampler") && mac.contains("nnan ninf"));
}

#[test]
fn the_fp_contract_pragma_stops_fmuladd() {
    let frag = |head: &str, body: &str| {
        emit(&format!(
            "{head}struct O {{ float4 c [[color(0)]]; }};\n\
             struct I {{ float4 u [[user(locn0)]]; }};\n\
             fragment O main0(I in [[stage_in]]) {{ O o; {body} return o; }}\n"
        ))
    };
    let mixed = frag(
        "",
        "float4 t; { \n#pragma clang fp contract(off)\n t = in.u * in.u + in.u; } o.c = t * in.u + in.u;",
    );
    assert_eq!(
        mixed.matches("@llvm.fmuladd.v4f32(<4 x float> %").count(),
        1,
        "{mixed}"
    );
    let file = frag(
        "#pragma clang fp contract(off)\n",
        "o.c = in.u * in.u + in.u; o.c += in.u * in.u;",
    );
    assert!(!file.contains("fmuladd"), "{file}");
}

#[test]
fn user_call_sites_and_derivatives_have_separate_attribute_groups() {
    let ir = emit(
        "struct O { float4 c0 [[color(0)]]; };\nstruct I { float4 u [[user(locn0)]]; };\n\
         float d(float x) { return dfdx(x); }\n\
         fragment O main0(I in [[stage_in]]) { O o = {}; o.c0 = float4(d(in.u.x), d(in.u.y), 0.0, 1.0); return o; }\n",
    );
    assert!(ir.contains("@_ZL1df(float noundef %v8) #11"), "{ir}");
    assert_eq!(ir.matches("attributes #11 ").count(), 1, "{ir}");
    assert!(ir.contains("attributes #11 = { convergent nobuiltin \"no-builtins\" }"));
    assert!(ir.contains("declare float @air.dfdx.f32(float) #12"));
    assert!(ir.contains("attributes #12 = { convergent nounwind willreturn }"));
}

#[test]
fn signed_arithmetic_wraps_without_nsw() {
    let ir = emit(
        "kernel void k(device int* p [[buffer(0)]], uint i [[thread_position_in_grid]])\n\
         { int a = p[0]; int b = p[1]; p[2] = a + b; p[3] = a - b; p[4] = a * b; p[5] = -a; }\n",
    );
    for op in ["add i32", "sub i32", "mul i32"] {
        assert!(ir.contains(op), "{op}\n{ir}");
    }
    assert!(!ir.contains("nsw") && !ir.contains("nuw"), "{ir}");
}
