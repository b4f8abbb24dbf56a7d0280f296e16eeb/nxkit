//! The AIR 2.8 comparison gather uses the scalar reference but returns four taps.

use air_emit::{compile_source, Target};

const SOURCE: &str = "#include <metal_stdlib>
using namespace metal;
struct Material { depth2d<float> depth_map [[id(0)]]; sampler s [[id(1)]]; };
fragment float4 main0(constant Material& m [[buffer(0)]], float4 pos [[position]]) {
    return m.depth_map.gather_compare(m.s, pos.xy, pos.z);
}";

#[test]
fn depth_gather_passes_ref_and_returns_vector() {
    let target = Target::macos_for(26);
    let ir = compile_source(SOURCE, &target, "t.metal").expect("emits");
    let name = "@air.gather_compare_depth_2d.f32";
    let call = ir
        .lines()
        .find(|line| line.contains("call") && line.contains(name))
        .unwrap();
    assert!(call.contains("call { <4 x float>, i8 }"), "{call}");
    assert!(call.contains("i32 1, <2 x float> %"), "{call}");
    assert!(call.contains("float %"), "{call}");
    assert!(
        call.contains("i1 true, <2 x i32> zeroinitializer, i32 0)"),
        "{call}"
    );
    assert!(ir.lines().any(|line| line.starts_with("declare")
        && line.contains(name)
        && line.contains("i32, <2 x float>, float, i1, <2 x i32>, i32)")));

    let with_offset = SOURCE.replace("pos.z)", "pos.z, int2(1, -1))");
    assert!(compile_source(&with_offset, &target, "t.metal")
        .is_err_and(|e| e.key().contains("gather_compare:depth2d-ref")));
    for target in [Target::macos(), Target::ios()] {
        assert!(compile_source(SOURCE, &target, "t.metal")
            .is_err_and(|e| e.key().contains("gather_compare:target")));
    }
}

#[test]
fn depth_cube_gather_passes_ref_without_an_offset_operand() {
    let source = SOURCE
        .replace("depth2d<float>", "depthcube<float>")
        .replace("pos.xy, pos.z", "pos.xyz, pos.w");
    let target = Target::macos_for(26);
    let ir = compile_source(&source, &target, "t.metal").expect("emits");
    let name = "@air.gather_compare_depth_cube.f32";
    let call = ir
        .lines()
        .find(|line| line.contains("call { <4 x float>, i8 }") && line.contains(name))
        .expect("cube compare gather");
    assert!(call.contains(", i32 1, <3 x float> %"), "{call}");
    assert!(
        call.contains(", float %") && call.contains(", i32 0)"),
        "{call}"
    );
    assert!(
        !call.contains("i1 true") && !call.contains("<3 x i32>"),
        "{call}"
    );
    let with_offset = source.replace("pos.w)", "pos.w, int3(0))");
    assert!(compile_source(&with_offset, &target, "t.metal").is_err());
    for target in [Target::macos(), Target::ios()] {
        assert!(compile_source(&source, &target, "t.metal")
            .is_err_and(|e| e.key().contains("gather_compare:target")));
    }
}

#[test]
fn depth_cube_array_gather_puts_slice_before_compare_ref() {
    let source = SOURCE
        .replace("depth2d<float>", "depthcube_array<float>")
        .replace("pos.xy, pos.z", "pos.xyz, 3u, pos.w");
    let target = Target::macos_for(26);
    let ir = compile_source(&source, &target, "t.metal").expect("emits");
    let name = "@air.gather_compare_depth_cube_array.f32";
    let call = ir
        .lines()
        .find(|line| line.contains("call { <4 x float>, i8 }") && line.contains(name))
        .expect("cube-array compare gather");
    assert!(call.contains(", i32 1, <3 x float> %"), "{call}");
    assert!(
        call.contains(", i32 3, float %") && call.contains(", i32 0)"),
        "{call}"
    );
    assert!(
        !call.contains("i1 true") && !call.contains("<3 x i32>"),
        "{call}"
    );
    for suffix in ["pos.w, int3(0))", "int(pos.w))"] {
        let bad = source.replace("pos.w)", suffix);
        assert!(compile_source(&bad, &target, "t.metal").is_err(), "{bad}");
    }
    for target in [Target::macos(), Target::ios()] {
        assert!(compile_source(&source, &target, "t.metal")
            .is_err_and(|e| e.key().contains("gather_compare:target")));
    }
}

#[test]
fn depth_plane_array_gather_matches_air28_operands_and_effects() {
    let source = SOURCE
        .replace("depth2d<float>", "depth2d_array<float>")
        .replace("pos.xy, pos.z", "pos.xy, 3u, pos.z");
    let target = Target::macos_for(26);
    let ir = compile_source(&source, &target, "t.metal").expect("emits");
    let name = "@air.gather_compare_depth_2d_array.f32";
    let call = ir
        .lines()
        .find(|line| line.contains("call { <4 x float>, i8 }") && line.contains(name))
        .expect("plane-array compare gather");
    assert!(call.contains(", i32 1, <2 x float> %"), "{call}");
    assert!(call.contains(", i32 3, float %"), "{call}");
    assert!(
        call.contains("i1 true, <2 x i32> zeroinitializer, i32 0)"),
        "{call}"
    );
    assert_eq!(call.matches("nocapture readonly").count(), 2, "{call}");
    let (_, group) = call.rsplit_once(") #").expect("call attributes");
    assert!(
        ir.contains(&format!(
            "attributes #{group} = {{ argmemonly nounwind willreturn }}"
        )),
        "{ir}"
    );
    let with_offset = source.replace("pos.z)", "pos.z, int2(0))");
    assert!(compile_source(&with_offset, &target, "t.metal").is_err());
    for target in [Target::macos(), Target::ios()] {
        assert!(compile_source(&source, &target, "t.metal")
            .is_err_and(|e| e.key().contains("gather_compare:target")));
    }
}
