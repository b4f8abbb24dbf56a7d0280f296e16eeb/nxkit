//! Explicit texture derivatives through both producer sampler paths.

use air_emit::{compile_source, Target};

const PRE: &str = "#include <metal_stdlib>\nusing namespace metal;\n";

#[test]
fn cube_gradient_uses_air28_layout_for_both_samplers() {
    check_gradients("cube", "float, i32)", "float 0x0000000000000000, i32 0)");
}

#[test]
fn volume_gradient_uses_air28_layout_for_both_samplers() {
    check_gradients(
        "3d",
        "float, i1, <3 x i32>, i32)",
        "float 0x0000000000000000, i1 true, <3 x i32> zeroinitializer, i32 0)",
    );
}

#[test]
fn plane_gradient_uses_air28_layout_for_both_samplers() {
    check_gradients(
        "2d",
        "float, i1, <2 x i32>, i32)",
        "float 0x0000000000000000, i1 true, <2 x i32> zeroinitializer, i32 0)",
    );
}

fn check_gradients(dim: &str, declaration_tail: &str, call_tail: &str) {
    let body = "struct dm_border_t { uint mask; float4 k[2]; };
        float4 dm_bmix(float4 a, float4 b, float4 k) { return a + b * k; }
        template<typename V, typename T, typename... A> static inline __attribute__((always_inline))
        V dm_bs(V r, constant dm_border_t* b, uint i, T t, sampler w, A... a) {
          if (b->mask >> i & 1u) r = dm_bmix(r, t.sample(w, a...), b->k[i]); return r;
        }
        struct Material { texturecube<float> tex [[id(0)]]; sampler s [[id(1)]];
          sampler wrap [[id(2)]]; constant dm_border_t* border [[id(3)]]; };
        fragment float4 main0(constant Material& a [[buffer(0)]]) {
          return dm_bs(a.tex.sample(a.s, float3(1.0f), gradientcube(float3(0.5f), float3(0.25f))),
              a.border, 0u, a.tex, a.wrap, float3(1.0f), gradientcube(float3(0.5f), float3(0.25f)));
        }";
    let mut source = format!("{PRE}{body}").replace("cube", dim);
    let lanes = if dim == "2d" {
        source = source.replace("float3(", "float2(");
        2
    } else {
        3
    };
    let target = Target::macos_for(26);
    let ir = compile_source(&source, &target, "t.metal").expect("emits");
    let intrinsic = format!("@air.sample_texture_{dim}_grad.v4f32");
    let calls: Vec<_> = ir
        .lines()
        .filter(|line| line.contains("call") && line.contains(&intrinsic))
        .collect();
    assert_eq!(calls.len(), 2, "{ir}");
    for call in calls {
        assert!(call.contains(call_tail), "{call}");
    }
    assert!(ir.lines().any(|line| line.starts_with("declare")
        && line.contains(&intrinsic)
        && line.contains(&format!(
            "<{lanes} x float>, <{lanes} x float>, <{lanes} x float>, {declaration_tail}"
        ))));

    let wrong_lanes = if lanes == 2 { 3 } else { 2 };
    let wrong_derivative = source.replace(
        &format!("float{lanes}(0.5f)"),
        &format!("float{wrong_lanes}(0.5f)"),
    );
    assert!(
        compile_source(&wrong_derivative, &target, "t.metal").is_err_and(|e| e
            .to_string()
            .contains(&format!("gradient{dim} requires float{lanes} derivatives")))
    );
    if dim == "2d" {
        let offset = source.replace(
            "gradient2d(float2(0.5f), float2(0.25f))",
            "gradient2d(float2(0.5f), float2(0.25f)), int2(1)",
        );
        assert!(compile_source(&offset, &target, "t.metal")
            .is_err_and(|e| e.key().contains("sample-option:gradient2d")));
    }
    for target in [Target::macos(), Target::ios()] {
        assert!(compile_source(&source, &target, "t.metal")
            .is_err_and(|e| e.key().contains(&format!("gradient{dim}:target"))));
    }
}

#[test]
fn cube_array_gradient_puts_layer_before_derivatives() {
    let source = format!(
        "{PRE}struct Material {{ texturecube_array<float> tex [[id(0)]]; sampler s [[id(1)]]; }};
        fragment float4 main0(constant Material& m [[buffer(0)]], float4 p [[position]]) {{
            return m.tex.sample(m.s, p.xyz, 3u, gradientcube(float3(0.5f), float3(0.25f)));
        }}"
    );
    let target = Target::macos_for(26);
    let ir = compile_source(&source, &target, "t.metal").expect("emits");
    let name = "@air.sample_texture_cube_array_grad.v4f32(";
    let call = ir
        .lines()
        .find(|line| line.contains("call { <4 x float>, i8 }") && line.contains(name))
        .expect("cube-array gradient call");
    assert!(
        call.contains("<3 x float> %") && call.contains(", i32 3, <3 x float> "),
        "{call}"
    );
    assert!(call.contains("float 0x0000000000000000, i32 0)"), "{call}");
    assert!(ir.lines().any(|line| line.starts_with("declare")
        && line.contains(name)
        && line.contains("<3 x float>, i32, <3 x float>, <3 x float>, float, i32)")));
    let offset = source.replace("float3(0.25f)));", "float3(0.25f)), int3(0));");
    assert!(compile_source(&offset, &target, "t.metal").is_err());
    for target in [Target::macos(), Target::ios()] {
        assert!(compile_source(&source, &target, "t.metal")
            .is_err_and(|e| e.key() == "unsupported:gradientcube:target"));
    }
}
