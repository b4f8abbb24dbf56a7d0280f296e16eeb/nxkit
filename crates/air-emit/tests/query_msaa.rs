//! Multisampled size and sample-count queries carry no mip argument.

use air_emit::{compile_source, Target};

const SOURCE: &str = "#include <metal_stdlib>
using namespace metal;
struct Material { texture2d_ms<float> color [[id(0)]]; };
fragment float4 main0(constant Material& m [[buffer(0)]]) {
    return float4(float(m.color.get_width()), float(m.color.get_height()),
                  float(m.color.get_num_samples()), 1.0f);
}";

#[test]
fn multisample_queries_use_only_the_texture_handle() {
    let target = Target::macos_for(26);
    let ir = compile_source(SOURCE, &target, "t.metal").expect("emits");
    for query in ["width", "height", "num_samples"] {
        let name = format!("@air.get_{query}_texture_2d_ms");
        let call = ir
            .lines()
            .find(|line| line.contains("call i32") && line.contains(&name))
            .unwrap();
        let (_, args) = call.split_once(&format!("{name}(")).unwrap();
        let (args, _) = args.rsplit_once(") #").unwrap();
        assert!(args.starts_with("%struct._texture_2d_ms_t addrspace(1)* nocapture readonly %"));
        assert!(!args.contains(','), "{call}");
    }
    assert!(!ir.contains("@air.get_read_sampler"));
    for source in [
        SOURCE.replace("get_width()", "get_width(2u)"),
        SOURCE.replace("get_num_samples()", "get_num_samples(1u)"),
        SOURCE.replace("get_height()", "get_num_mip_levels()"),
        SOURCE.replace("texture2d_ms<float>", "texture2d<float>"),
    ] {
        assert!(compile_source(&source, &target, "t.metal").is_err());
    }
    for target in [Target::macos(), Target::ios()] {
        assert!(compile_source(SOURCE, &target, "t.metal")
            .is_err_and(|e| e.key().contains("texture2d_ms:target")));
    }
}

#[test]
fn ordinary_dimensions_keep_the_mip_operand() {
    let source = SOURCE
        .replace("texture2d_ms<float>", "texture2d<float>")
        .replace("get_width()", "get_width(2u)")
        .replace("get_height()", "get_height(2u)")
        .replace("float(m.color.get_num_samples())", "0.0f");
    for target in [Target::macos_for(26), Target::macos_for(27), Target::ios()] {
        let ir = compile_source(&source, &target, "t.metal").expect("emits");
        for query in ["width", "height"] {
            let name = format!("@air.get_{query}_texture_2d(");
            let call = ir
                .lines()
                .find(|line| line.contains("call i32") && line.contains(&name))
                .unwrap();
            assert!(call.contains(", i32 2)"), "{call}");
        }
    }
}

#[test]
fn multisample_array_queries_use_only_the_texture_handle() {
    let source = SOURCE
        .replace("texture2d_ms<float>", "texture2d_ms_array<float>")
        .replace("1.0f", "float(m.color.get_array_size())");
    let target = Target::macos_for(26);
    let ir = compile_source(&source, &target, "t.metal").expect("emits");
    for query in ["width", "height", "array_size", "num_samples"] {
        let name = format!("@air.get_{query}_texture_2d_ms_array(");
        let call = ir
            .lines()
            .find(|line| line.contains("call i32") && line.contains(&name))
            .expect("multisample array query");
        let (_, args) = call.split_once(&name).unwrap();
        let (args, _) = args.rsplit_once(") #").unwrap();
        assert!(
            args.starts_with("%struct._texture_2d_ms_array_t addrspace(1)* nocapture readonly %")
        );
        assert!(!args.contains(','), "{call}");
    }
    for replacement in [
        "float(m.color.get_width(0u))",
        "float(m.color.get_num_mip_levels())",
        "m.color.read(uint2(0u), 0u, 0u).x",
    ] {
        let bad = source.replace("float(m.color.get_width())", replacement);
        assert!(compile_source(&bad, &target, "t.metal").is_err(), "{bad}");
    }
    for target in [Target::macos_for(27), Target::ios()] {
        assert!(compile_source(&source, &target, "t.metal")
            .is_err_and(|e| e.key() == "unsupported:texture2d_ms_array:target"));
    }
}
