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
fn device_offsets_use_wrapping_pointer_arithmetic() {
    let ir = emit(
        "kernel void k(device uint* out [[buffer(0)]], uint i [[thread_position_in_grid]]) {
        out[1] = out[i];
    }",
    );
    assert_eq!(
        ir.matches("getelementptr i32, i32 addrspace(1)*").count(),
        2,
        "{ir}"
    );
    assert!(
        !ir.contains("getelementptr inbounds i32, i32 addrspace(1)*"),
        "{ir}"
    );
}

#[test]
fn dynamic_array_index_is_not_inbounds_in_any_space() {
    let ir = emit(
        "kernel void k(device uint* out [[buffer(0)]], uint i [[thread_position_in_grid]]) {
        uint loc[4]; loc[0] = out[2]; loc[1] = out[3]; loc[2] = out[4]; loc[3] = out[5];
        out[1] = loc[i & 3u];
    }",
    );
    assert_eq!(
        ir.matches("getelementptr inbounds [4 x i32], [4 x i32]*")
            .count(),
        4,
        "{ir}"
    );
    assert_eq!(
        ir.matches("getelementptr [4 x i32], [4 x i32]*").count(),
        1,
        "{ir}"
    );
}

#[test]
fn thread_array_reference_parameter_indexes_the_callers_storage() {
    let ir = emit(
        "static void write(thread uint (&values)[4], uint i, uint v) { values[i] = v; }\n\
         kernel void main0(device uint* out [[buffer(0)]]) {\n\
             uint values[4]; write(values, 1u, out[0]); out[1] = values[1];\n\
         }\n",
    );
    assert!(ir.contains("[4 x i32]*"), "{ir}");
    assert!(ir.contains("getelementptr inbounds [4 x i32]"), "{ir}");
    assert!(ir.contains("call void"), "{ir}");
}

#[test]
fn address_space_value_without_array_reference_is_still_rejected() {
    for parameter in ["thread uint value", "threadgroup uint value"] {
        let src = format!("{PRE}static void write({parameter}) {{}}");
        let error = compile_source(&src, &Target::macos(), "t").unwrap_err();
        assert!(
            error
                .key()
                .starts_with("unsupported:rejected:addrspace-value:"),
            "{error:?}"
        );
    }
}

#[test]
fn private_array_cannot_bind_to_threadgroup_reference() {
    let src = format!(
        "{PRE}static uint read_slot(threadgroup uint (&values)[4]) {{ return values[0]; }}\n\
         kernel void main0(device uint* out [[buffer(0)]]) {{ uint values[4]; out[0] = read_slot(values); }}"
    );
    let error = compile_source(&src, &Target::macos(), "t").unwrap_err();
    assert!(error.key().contains("overload-resolution"), "{error:?}");
}

#[test]
fn const_array_reference_falls_back_instead_of_losing_the_qualifier() {
    for space in ["thread", "threadgroup"] {
        let source =
            format!("{PRE}static uint read(const {space} uint (&a)[2]) {{ return a[0]; }}");
        let error = compile_source(&source, &Target::macos(), "t").unwrap_err();
        assert!(error.key().contains("const-array-reference"), "{error:?}");
    }
}

#[test]
fn array_reference_overload_follows_argument_address_space() {
    let private = "static uint read_slot(thread uint (&a)[2]) { return a[0]; }";
    let shared = "static uint read_slot(threadgroup uint (&a)[2]) { return a[1]; }";
    for helpers in [[private, shared], [shared, private]] {
        let ir = emit(&format!(
            "{}\n{}\nkernel void k(device uint* out [[buffer(0)]]) {{ \
             uint a[2]; threadgroup uint b[2]; a[0] = out[0]; b[1] = out[1]; \
             out[0] = read_slot(a); out[1] = read_slot(b); }}",
            helpers[0], helpers[1]
        ));
        let calls: Vec<_> = ir
            .lines()
            .filter(|l| l.contains("call i32 @") && l.contains("[2 x i32]"))
            .collect();
        assert_eq!(calls.len(), 2, "{ir}");
        assert!(calls[0].contains("[2 x i32]*"), "{ir}");
        assert!(calls[1].contains("[2 x i32] addrspace(3)*"), "{ir}");
    }
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
    assert!(mac.contains("air.get_read_sampler"));
    assert!(!mac.contains("nnan") && !mac.contains("no-nans-fp-math"));
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
    assert!(!file.contains("invariance-late-contract"), "{file}");
    assert!(mixed.contains("invariance-late-contract"), "{mixed}");
}

#[test]
fn file_contract_pragma_only_changes_float_value_helpers() {
    let ir = emit(
        "#pragma clang fp contract(off)
         static uint float_helper(float x) { return as_type<uint>(x); }
         static uint uint_helper(uint x) { return x + 1u; }
         struct I { float4 u [[user(locn0)]]; };
         fragment float4 main0(I in [[stage_in]]) {
             return float4(as_type<float>(uint_helper(float_helper(in.u.x))));
         }",
    );
    let attrs = |name: &str| {
        let head = ir
            .lines()
            .find(|line| line.starts_with("define ") && line.contains(name))
            .expect("definition");
        let (_, group) = head.rsplit_once(" #").expect("group");
        let group = group.trim_end_matches(" {");
        let prefix = format!("attributes #{group} =");
        ir.lines()
            .find(|line| line.starts_with(&prefix))
            .expect("attributes")
    };
    assert!(!attrs("float_helper").contains("invariance-late-contract"));
    assert!(attrs("uint_helper").contains("invariance-late-contract"));
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

#[test]
fn the_blend_rule_of_a_pixel_shader_emits() {
    let ir = emit(
        "struct spvDescriptorSetBuffer0 { constant uint* dm_bz [[id(0)]]; };\n\
         struct main0_in { float4 u [[user(locn0)]]; };\n\
         struct main0_out { float4 c0 [[color(0)]]; float4 c1 [[color(1)]]; };\n\
         static inline __attribute__((always_inline)) bool dm_bz_zero(uint k, float a, float a1)\n\
         {\n\
             uint b = k < 4u ? as_type<uint>(a) : as_type<uint>(a1);\n\
             return k == 1u || ((k == 2u || k == 4u) && (b & 0x7fffffffu) == 0u) || ((k == 3u || k == 5u) && b == 0x3f800000u);\n\
         }\n\
         static main0_out dm_body_main0(main0_in in, constant spvDescriptorSetBuffer0& spvDescriptorSet0)\n\
         {\n\
             main0_out out = {};\n\
             out.c0 = in.u;\n\
             out.c1 = in.u * in.u;\n\
             return out;\n\
         }\n\
         fragment main0_out main0(main0_in in [[stage_in]], constant spvDescriptorSetBuffer0& spvDescriptorSet0 [[buffer(0)]])\n\
         {\n\
             main0_out dm_r = dm_body_main0(in, spvDescriptorSet0);\n\
             uint dm_bz = *spvDescriptorSet0.dm_bz;\n\
             if (dm_bz != 0u)\n\
             {\n\
                 if (dm_bz_zero((dm_bz >> 0u) & 7u, dm_r.c0.w, 0.0f)) { dm_r.c0.x = 0.0f; dm_r.c0.y = 0.0f; dm_r.c0.z = 0.0f; }\n\
                 if (dm_bz_zero((dm_bz >> 3u) & 7u, dm_r.c1.w, dm_r.c0.w)) { dm_r.c1.x = 0.0f; dm_r.c1.y = 0.0f; dm_r.c1.z = 0.0f; }\n\
             }\n\
             return dm_r;\n\
         }\n",
    );
    assert!(
        ir.contains("define <{ <4 x float>, <4 x float> }> @main0("),
        "{ir}"
    );
    // The tests are on integer bits, so fast math cannot fold them away.
    assert!(ir.contains("and i32"), "{ir}");
    assert!(ir.contains("1065353216"), "{ir}");
}

const SAMPLE_GRAD_2D_ARRAY: &str = "air.sample_texture_2d_array_grad.v4f32";

// Distinct, exactly representable components from the authored gradient2d fixtures.
const GRADIENT_DX: [&str; 2] = ["0x3FB0000000000000", "0xBFD0000000000000"]; // (1/16, -1/4)
const GRADIENT_DY: [&str; 2] = ["0x3FE0000000000000", "0x3FC0000000000000"]; // (1/2, 1/8)

fn vector_value<'a>(ir: &'a str, element_ty: &str, values: [&str; 2]) -> &'a str {
    let first = format!(
        "insertelement <2 x {element_ty}> undef, {element_ty} {}, i32 0",
        values[0]
    );
    let base = result_of(ir, &first);
    let second = format!(
        "insertelement <2 x {element_ty}> {base}, {element_ty} {}, i32 1",
        values[1]
    );
    result_of(ir, &second)
}

fn result_of<'a>(ir: &'a str, operation: &str) -> &'a str {
    ir.lines()
        .find_map(|line| {
            let (result, op) = line.trim().split_once(" = ")?;
            op.starts_with(operation).then_some(result)
        })
        .unwrap_or_else(|| panic!("missing {operation} in {ir}"))
}

fn gradient_call(ir: &str) -> &str {
    ir.lines()
        .find(|line| line.contains("call") && line.contains(SAMPLE_GRAD_2D_ARRAY))
        .unwrap()
}

#[test]
fn array_gradient_preserves_derivatives_and_offset() {
    let ir = emit(
        "struct spvDescriptorSetBuffer0 { texture2d_array<float> tex [[id(0)]]; sampler s [[id(1)]]; };\n\
         fragment float4 main0(constant spvDescriptorSetBuffer0& a [[buffer(0)]]) {\n\
           return a.tex.sample(a.s, float2(0.25f, 0.5f), 1u,\n\
                               gradient2d(float2(0.0625f, -0.25f), float2(0.5f, 0.125f)), int2(2, -3));\n\
         }",
    );
    let dx = vector_value(&ir, "float", GRADIENT_DX);
    let dy = vector_value(&ir, "float", GRADIENT_DY);
    let offset = vector_value(&ir, "i32", ["2", "-3"]);
    let call = gradient_call(&ir);
    assert!(call.contains(&format!(
        "i32 1, <2 x float> {dx}, <2 x float> {dy}, float 0x0000000000000000, i1 true, <2 x i32> {offset}, i32 0)"
    )), "{call}");
    let decl = ir
        .lines()
        .find(|s| s.starts_with("declare") && s.contains(SAMPLE_GRAD_2D_ARRAY))
        .unwrap();
    assert!(
        decl.contains("<2 x float>, i32, <2 x float>, <2 x float>, float, i1, <2 x i32>, i32"),
        "{decl}"
    );
    assert!(ir.contains("extractvalue { <4 x float>, i8 }"));
}

#[test]
fn array_gradient_passes_both_derivatives_through_border_helper() {
    let ir = emit(
        "struct dm_border_t { uint mask; float4 k[2]; };\n\
         float4 dm_bmix(float4 a, float4 b, float4 k) { return a + b * k; }\n\
         template<typename V, typename T, typename... A> static inline __attribute__((always_inline))\n\
         V dm_bs(V r, constant dm_border_t* b, uint i, T t, sampler w, A... a) {\n\
           if (b->mask >> i & 1u) r = dm_bmix(r, t.sample(w, a...), b->k[i]); return r; }\n\
         struct spvDescriptorSetBuffer0 { texture2d_array<float> tex [[id(0)]]; sampler s [[id(1)]];\n\
           constant dm_border_t* border [[id(2)]]; };\n\
         fragment float4 main0(constant spvDescriptorSetBuffer0& a [[buffer(0)]]) {\n\
           return dm_bs(float4(0.0f), a.border, 0u, a.tex, a.s, float2(0.5f), 1u,\n\
                        gradient2d(float2(0.0625f, -0.25f), float2(0.5f, 0.125f))); }",
    );
    let (caller, helper) = ir.split_once("define internal <4 x float> @dm_bs").unwrap();
    let dx = vector_value(caller, "float", GRADIENT_DX);
    let dy = vector_value(caller, "float", GRADIENT_DY);
    let forward = caller
        .lines()
        .find(|line| line.contains("call") && line.contains("@dm_bs"))
        .unwrap();
    assert!(
        forward.contains(&format!(
            "i32 noundef 1, <2 x float> noundef {dx}, <2 x float> noundef {dy})"
        )),
        "{forward}"
    );

    let reload = |arg| {
        let store = format!("store <2 x float> {arg}, <2 x float>* ");
        let slot = helper
            .lines()
            .find_map(|line| line.trim().strip_prefix(&store))
            .unwrap()
            .split_once(',')
            .unwrap()
            .0;
        result_of(helper, &format!("load <2 x float>, <2 x float>* {slot},"))
    };
    // The specialization's first derivative is argument 7, its second is argument 8.
    let dx = reload("%a7");
    let dy = reload("%a8");
    let call = gradient_call(helper);
    assert!(
        call.contains(&format!("<2 x float> {dx}, <2 x float> {dy}, float 0x0000000000000000, i1 true, <2 x i32> zeroinitializer, i32 0)")),
        "{call}"
    );
}

#[test]
fn array_gradient_rejects_scalar_derivatives() {
    let source = format!(
        "{PRE}struct spvDescriptorSetBuffer0 {{ texture2d_array<float> tex [[id(0)]]; sampler s [[id(1)]]; }};\n\
         fragment float4 main0(constant spvDescriptorSetBuffer0& a [[buffer(0)]]) {{\n\
            return a.tex.sample(a.s, float2(0.5f), 1u, gradient2d(0.0625f, float2(0.0f))); }}"
    );
    let err = compile_source(&source, &Target::macos(), "t.metal").unwrap_err();
    assert!(
        format!("{err:?}").contains("gradient2d requires float2 derivatives"),
        "{err:?}"
    );
}

#[test]
fn fragment_sample_id_can_be_a_stage_in_member() {
    let body = "struct I { uint gl_SampleID [[sample_id]]; };
        struct O { float4 c [[color(0)]]; };
        fragment O main0(I in [[stage_in]]) {
            O o;
            o.c = float4(float(in.gl_SampleID));
            return o;
        }";
    for target in [Target::macos(), Target::ios()] {
        let ir = compile_source(&format!("{PRE}{body}"), &target, "t.metal").expect("emits");
        assert!(
            ir.contains(
                "i32 0, !\"air.sample_id\", !\"air.arg_type_name\", !\"uint\", !\"air.arg_name\", !\"gl_SampleID\""
            ),
            "{ir}"
        );
        assert!(ir.contains("@air.convert.f.f32.u.i32(i32 %"), "{ir}");

        let bad = body.replace("uint gl_SampleID", "float gl_SampleID");
        assert!(compile_source(&format!("{PRE}{bad}"), &target, "t.metal")
            .is_err_and(|e| e.to_string().contains("sample_id field is not uint")));
    }
}

#[test]
fn viewport_array_index_matches_air28_entry_descriptors() {
    let body = "struct V { float4 pos [[position]]; uint viewport [[viewport_array_index]]; };
        vertex V vs(uint instance [[instance_id]]) {
            V out;
            out.pos = float4(0.0f);
            out.viewport = instance;
            return out;
        }
        fragment float4 fs(V in [[stage_in]]) { return float4(float(in.viewport)); }";
    let target = Target::macos_for(26);
    let source = format!("{PRE}{body}");
    let ir = compile_source(&source, &target, "t.metal").expect("emits");
    let md = r#"!"air.viewport_array_index", !"air.arg_type_name", !"uint", !"air.arg_name", !"viewport"}"#;
    for prefix in ["", "i32 1, "] {
        assert!(ir.contains(&format!("!{{{prefix}{md}")), "{ir}");
    }
    for bad in [
        source.replace("uint viewport", "int viewport"),
        source.replace("[[viewport_array_index]]", "[[viewport_array_index(0)]]"),
        source.replace(
            "[[viewport_array_index]]",
            "[[viewport_array_index, render_target_array_index]]",
        ),
    ] {
        assert!(compile_source(&bad, &target, "t.metal").is_err_and(|e| e
            .to_string()
            .contains("viewport_array_index requires a uint field")));
    }
    for target in [Target::macos(), Target::ios()] {
        assert!(compile_source(&source, &target, "t.metal")
            .is_err_and(|e| e.key().contains("viewport_array_index:target")));
    }
}

#[test]
fn air28_viewport_packed_return_does_not_count_as_a_vector() {
    let body = "struct V { float4 pos [[position]]; uint viewport [[viewport_array_index]]; };
        vertex V vp(uint i [[vertex_id]]) { V out; out.pos = float4(0.0f);
            out.viewport = i; return out; }
        struct In { float4 pos [[attribute(0)]]; };
        vertex V vp_param(In in [[stage_in]]) { V out; out.pos = in.pos;
            out.viewport = 0u; return out; }
        vertex V vp_call(uint i [[vertex_id]]) { V out; out.pos = sqrt(float4(float(i)));
            out.viewport = i; return out; }
        struct Ordinary { float4 pos [[position]]; };
        vertex Ordinary plain() { Ordinary out; out.pos = float4(0.0f); return out; }
        fragment float4 fs(V in [[stage_in]]) { return float4(float(in.viewport)); }";
    let ir =
        compile_source(&format!("{PRE}{body}"), &Target::macos_for(26), "t.metal").expect("emits");
    for (name, width) in [
        ("vp", 0),
        ("vp_param", 128),
        ("vp_call", 128),
        ("plain", 128),
        ("fs", 128),
    ] {
        let prefix = format!("@{name}(");
        let def = ir
            .lines()
            .find(|line| line.starts_with("define ") && line.contains(&prefix))
            .expect("entry");
        let group = def.rsplit_once(" #").expect("definition attributes").1;
        let group = group.split_whitespace().next().expect("attribute group");
        let prefix = format!("attributes #{group} = ");
        let attrs = ir
            .lines()
            .find(|line| line.starts_with(&prefix))
            .expect("attribute definition");
        assert!(
            attrs.contains(&format!("\"min-legal-vector-width\"=\"{width}\"")),
            "{name}: {attrs}"
        );
    }
}
