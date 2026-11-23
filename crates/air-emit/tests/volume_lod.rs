//! The sampled float volume and cube LOD queries are fragment-only on AIR 2.8.

use air_emit::{compile_source, Target};

const PRE: &str = "#include <metal_stdlib>
using namespace metal;
struct Material { texture3d<float> tex [[id(0)]]; sampler s [[id(1)]]; };
static float query(constant Material& m, float3 uvw) {
    return m.tex.calculate_clamped_lod(m.s, uvw)
        + m.tex.calculate_unclamped_lod(m.s, uvw);
}
";

#[test]
fn volume_and_cube_lod_use_live_float3_and_both_air28_intrinsics() {
    for (kind, air_kind) in [
        ("texture3d", "texture_3d"),
        ("texturecube", "texture_cube"),
        ("texturecube_array", "texture_cube_array"),
    ] {
        check_lod(kind, air_kind);
    }
}

fn check_lod(kind: &str, air_kind: &str) {
    let source = format!(
        "{PRE}fragment float4 main0(constant Material& m [[buffer(0)]], float4 p [[position]]) {{
             return float4(query(m, p.xyz));
         }}"
    );
    let source = source.replace("texture3d", kind);
    let ir = compile_source(&source, &Target::macos_for(26), "t.metal").expect("emits");
    for mode in ["clamped", "unclamped"] {
        let name = format!("@air.calculate_{mode}_lod_{air_kind}");
        let call = ir
            .lines()
            .find(|line| line.contains("call ") && line.contains(&name))
            .expect("LOD call");
        let (_, operands) = call.split_once(&format!("{name}(")).expect("LOD operands");
        let (operands, _) = operands.rsplit_once(") #").expect("end of LOD operands");
        assert_eq!(operands.split(", ").count(), 4, "{call}");
        assert!(operands.contains("<3 x float> %"), "{call}");
        let load = sampler_load(&ir, call);
        assert!(!load.contains("!tbaa.struct"), "{load}");
        assert!(call.contains(", i32 0) #"), "{call}");
        let (_, group) = call.rsplit_once('#').expect("call attribute group");
        let prefix = format!("attributes #{group} =");
        let attrs = ir
            .lines()
            .find(|line| line.starts_with(&prefix))
            .expect("LOD call attributes");
        assert!(attrs.contains("convergent"), "{attrs}");
    }
    if kind == "texturecube_array" {
        let with_slice = source.replace("m.s, uvw)", "m.s, uvw, 3u)");
        assert!(compile_source(&with_slice, &Target::macos_for(26), "t.metal").is_err());
    }
    let unsupported = format!("unsupported:lod:{kind}");
    for target in [Target::macos(), Target::ios()] {
        assert!(compile_source(&source, &target, "t.metal").is_err_and(|e| e.key() == unsupported));
    }
}

#[test]
fn plane_lod_and_sampling_keep_the_sampler_copy_tag() {
    let source = "#include <metal_stdlib>
using namespace metal;
struct Material { texture2d<float> tex [[id(0)]]; sampler s [[id(1)]]; };
static float4 query(constant Material& m, float2 uv) {
    return m.tex.sample(m.s, uv) + float4(m.tex.calculate_clamped_lod(m.s, uv));
}
fragment float4 main0(constant Material& m [[buffer(0)]], float4 p [[position]]) {
    return query(m, p.xy);
}";
    let ir = compile_source(source, &Target::macos_for(26), "t.metal").expect("emits");
    for name in [
        "@air.sample_texture_2d.",
        "@air.calculate_clamped_lod_texture_2d(",
    ] {
        let call = ir
            .lines()
            .find(|line| line.contains("call ") && line.contains(name))
            .expect("texture call");
        let load = sampler_load(&ir, call);
        assert!(load.contains("!tbaa.struct"), "{load}");
    }
}

fn sampler_load<'a>(ir: &'a str, call: &str) -> &'a str {
    let (_, args) = call
        .split_once("%struct._sampler_t addrspace(2)* nocapture readonly ")
        .expect("sampler argument");
    let (id, _) = args.split_once(',').expect("sampler argument value");
    let prefix = format!("{id} = load %struct._sampler_t addrspace(2)*,");
    ir.lines()
        .find(|line| line.trim_start().starts_with(&prefix))
        .expect("sampler handle load")
}

#[test]
fn volume_and_cube_lod_through_a_helper_are_rejected_in_compute() {
    let source = format!(
        "{PRE}kernel void k(constant Material& m [[buffer(0)]], device float* out [[buffer(1)]]) {{
             *out = query(m, float3(0.5f));
         }}"
    );
    for kind in ["texture3d", "texturecube", "texturecube_array"] {
        let source = source.replace("texture3d", kind);
        let unsupported = format!("unsupported:lod:{kind}-stage");
        assert!(compile_source(&source, &Target::macos_for(26), "t.metal")
            .is_err_and(|e| e.key() == unsupported));
    }
}
