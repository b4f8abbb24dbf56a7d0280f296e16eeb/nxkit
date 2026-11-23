//! A volume read keeps the Z coordinate and mip in separate AIR operands.

use air_emit::{compile_source, Target};

const SOURCE: &str = "#include <metal_stdlib>
using namespace metal;
struct Material { texture3d<float> volume [[id(0)]]; };
fragment float4 main0(constant Material& m [[buffer(0)]], float4 p [[position]]) {
    return m.volume.read(uint3(p.xyz), uint(p.w));
}";

#[test]
fn volume_read_passes_int3_coord_offset_and_mip() {
    let target = Target::macos_for(26);
    let ir = compile_source(SOURCE, &target, "t.metal").expect("emits");
    let call = ir
        .lines()
        .find(|line| line.contains("call") && line.contains("@air.read_texture_3d.v4f32"))
        .unwrap();
    assert!(ir.contains("call %struct._sampler_t addrspace(2)* @air.get_read_sampler()"));
    assert!(call.contains("call { <4 x float>, i8 }"), "{call}");
    let (_, args) = call.split_once("<3 x i32> %").unwrap();
    let (coord, mip) = args
        .split_once(", <3 x i32> zeroinitializer, i32 %")
        .unwrap();
    let (mip, _) = mip.split_once(", i32 0)").unwrap();
    let definition = |id| {
        ir.lines()
            .find_map(|line| line.trim().strip_prefix(&format!("%{id} = ")))
            .unwrap()
    };
    let (_, xyz) = definition(coord)
        .split_once("@air.convert.u.v3i32.f.v3f32(<3 x float> %")
        .unwrap();
    let (_, w) = definition(mip)
        .split_once("@air.convert.u.i32.f.f32(float %")
        .unwrap();
    assert!(definition(xyz.split_once(')').unwrap().0).ends_with("<i32 0, i32 1, i32 2>"));
    assert!(definition(w.split_once(')').unwrap().0).ends_with(", i32 3"));
    assert!(ir.contains("extractvalue { <4 x float>, i8 }"));
    let extra_arg = SOURCE.replace("uint(p.w))", "uint(p.w), int3(0))");
    assert!(compile_source(&extra_arg, &target, "t.metal")
        .is_err_and(|e| e.key().contains("read:texture3d-args")));
    for target in [Target::macos(), Target::ios()] {
        assert!(compile_source(SOURCE, &target, "t.metal")
            .is_err_and(|e| e.key().contains("read:texture3d")));
    }
}
