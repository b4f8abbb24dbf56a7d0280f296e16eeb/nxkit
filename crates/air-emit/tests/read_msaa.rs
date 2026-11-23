//! Multisample reads take a sample index, not a mip level or an offset.

use air_emit::{compile_source, Target};

const SOURCE: &str = "#include <metal_stdlib>
using namespace metal;
struct Material { texture2d_ms<float> color [[id(0)]]; };
fragment float4 main0(constant Material& m [[buffer(0)]], float4 p [[position]]) {
    return m.color.read(uint2(p.xy), uint(p.z));
}";

#[test]
fn multisample_read_uses_read_access_and_sample_index() {
    let target = Target::macos_for(26);
    let ir = compile_source(SOURCE, &target, "t.metal").expect("emits");
    let call = ir
        .lines()
        .find(|line| line.contains("call") && line.contains("@air.read_texture_2d_ms.v4f32"))
        .unwrap();
    assert!(ir.contains("call %struct._sampler_t addrspace(2)* @air.get_read_sampler()"));
    assert!(ir.contains(r#"!"air.read", !"air.arg_type_name", !"texture2d_ms<float, read>""#));
    assert!(call.contains("call { <4 x float>, i8 }"), "{call}");
    let (_, args) = call.split_once("<2 x i32> %").unwrap();
    let (_, sample) = args.split_once(", i32 %").unwrap();
    let (sample, _) = sample.split_once(", i32 1)").unwrap();
    let definition = |id| {
        ir.lines()
            .find_map(|line| line.trim().strip_prefix(&format!("%{id} = ")))
            .unwrap()
    };
    let (_, z) = definition(sample)
        .split_once("@air.convert.u.i32.f.f32(float %")
        .unwrap();
    assert!(definition(z.split_once(')').unwrap().0).ends_with(", i32 2"));
    for target in [Target::macos(), Target::ios()] {
        let unused = SOURCE.replace("m.color.read(uint2(p.xy), uint(p.z))", "float4(0.0f)");
        for source in [SOURCE, &unused] {
            assert!(compile_source(source, &target, "t.metal")
                .is_err_and(|e| e.key().contains("texture2d_ms:target")));
        }
    }
    for unmatched in [
        SOURCE.replace("uint(p.z)", "uint(p.z), 1u"),
        SOURCE.replace("uint(p.z)", "p.z"),
        SOURCE.replace("uint2(p.xy), uint(p.z)", "uint2(p.xy)"),
        SOURCE.replace("texture2d_ms<float>", "texture2d_ms<float, access::sample>"),
        SOURCE.replace("texture2d_ms<float>", "texture2d_ms<uint>"),
    ] {
        assert!(compile_source(&unmatched, &target, "t.metal").is_err());
    }
}
