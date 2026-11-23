//! Float and unsigned texel buffers have a distinct AIR 2.8 read and write ABI.

use air_emit::{compile_source, Target};

const SOURCE: &str = "#include <metal_stdlib>
using namespace metal;
struct Material { texture_buffer<float> texels [[id(0)]]; };
fragment float4 main0(constant Material& m [[buffer(0)]], float4 p [[position]]) {
    return m.texels.read(uint(p.x));
}";

#[test]
fn read_only_texel_buffer_has_a_scalar_index_and_no_mip() {
    let target = Target::macos_for(26);
    let ir = compile_source(SOURCE, &target, "t.metal").expect("emits");
    assert!(
        ir.contains(
            "%\"struct.metal::texture_buffer\" = type { %struct._texture_buffer_1d_t addrspace(1)* }"
        ),
        "{ir}"
    );
    assert!(ir.contains("!\"air.read\", !\"air.arg_type_name\", !\"texture_buffer<float, read>\""));
    let intrinsic = "@air.read_texture_buffer_1d.v4f32";
    let call = ir
        .lines()
        .find(|line| line.contains("call ") && line.contains(intrinsic))
        .expect("buffer read");
    assert!(call.contains("call { <4 x float>, i8 }"), "{call}");
    assert!(call.contains("%struct._texture_buffer_1d_t addrspace(1)* nocapture readonly"));
    assert!(call.contains("%struct._sampler_t addrspace(2)* %"));
    assert!(call.contains(", i32 1) #"), "{call}");
    assert_eq!(call.matches("i32 ").count(), 2, "{call}");
    assert!(ir.contains("@air.get_read_sampler()"));
    assert!(ir.contains("extractvalue { <4 x float>, i8 }"));
}

#[test]
fn texel_buffer_rejects_unproven_forms_even_when_unused() {
    let target = Target::macos_for(26);
    for input in [
        SOURCE.replace("uint(p.x)", "int(p.x)"),
        SOURCE.replace("uint(p.x)", "uint(p.x), 0u"),
        SOURCE.replace(
            "texture_buffer<float>",
            "texture_buffer<float, access::write>",
        ),
        SOURCE
            .replace("texture_buffer<float>", "texture_buffer<int>")
            .replace("fragment float4", "fragment int4"),
    ] {
        assert!(
            compile_source(&input, &target, "t.metal").is_err(),
            "{input}"
        );
    }
    let unused = SOURCE.replace("m.texels.read(uint(p.x))", "float4(0.0f)");
    for target in [Target::macos(), Target::ios()] {
        for source in [SOURCE, &unused] {
            assert!(compile_source(source, &target, "t.metal")
                .is_err_and(|e| e.key() == "unsupported:texture_buffer:target"));
        }
    }
}

const READ_WRITE: &str = "#include <metal_stdlib>
using namespace metal;
struct Material { texture_buffer<float, access::read_write> texels [[id(0)]]; };
kernel void main0(constant Material& m [[buffer(0)]], uint lane [[thread_position_in_grid]]) {
    float4 color = m.texels.read(lane);
    m.texels.write(color, lane + 1u);
}";

#[test]
fn read_write_texel_buffer_matches_air28_operands() {
    let target = Target::macos_for(26);
    let ir = compile_source(READ_WRITE, &target, "t.metal").expect("emits");
    assert!(ir.contains(
        "!\"air.read_write\", !\"air.arg_type_name\", !\"texture_buffer<float, read_write>\""
    ));
    let read = ir
        .lines()
        .find(|l| l.contains("call ") && l.contains("@air.read_texture_buffer_1d.v4f32"))
        .expect("texel load");
    assert!(read.contains("nocapture readonly"), "{read}");
    assert!(read.contains("%struct._sampler_t addrspace(2)*"), "{read}");
    assert_eq!(read.matches("i32 ").count(), 2, "{read}");
    assert!(read.contains(", i32 3) #"), "{read}");
    let write = ir
        .lines()
        .find(|l| l.contains("call void @air.write_texture_buffer_1d.v4f32"))
        .expect("texel store");
    assert!(write.contains("%struct._texture_buffer_1d_t addrspace(1)* nocapture %"));
    assert_eq!(write.matches("i32 ").count(), 2, "{write}");
    assert!(!write.contains("sampler"));
    assert!(write.contains(", <4 x float> %"));
    assert!(write.contains(", i32 3) #"), "{write}");
}

#[test]
fn read_write_texel_buffer_rejects_unproved_methods_and_targets() {
    let target = Target::macos_for(26);
    for source in [
        READ_WRITE.replace("lane + 1u", "int(lane)"),
        READ_WRITE.replace("color, lane", "color.x, lane"),
        READ_WRITE.replace("lane + 1u", "lane, 0u"),
        READ_WRITE.replace("read(lane)", "get_width()"),
        READ_WRITE.replace(", access::read_write", ""),
    ] {
        assert!(
            compile_source(&source, &target, "t.metal").is_err(),
            "{source}"
        );
    }
    let unused = READ_WRITE.replace(
        "float4 color = m.texels.read(lane);\n    m.texels.write(color, lane + 1u);",
        "",
    );
    for target in [Target::macos(), Target::ios()] {
        assert!(compile_source(&unused, &target, "t.metal")
            .is_err_and(|e| e.key() == "unsupported:texture_buffer:target"));
    }
}

#[test]
fn read_only_texel_buffer_width_has_only_the_resource_operand() {
    let source = SOURCE.replace(
        "m.texels.read(uint(p.x))",
        "float4(float(m.texels.get_width()))",
    );
    let target = Target::macos_for(26);
    let ir = compile_source(&source, &target, "t.metal").expect("emits");
    let call = ir
        .lines()
        .find(|line| line.contains("call i32 @air.get_width_texture_buffer_1d("))
        .expect("buffer width");
    assert!(call.contains("%struct._texture_buffer_1d_t addrspace(1)* nocapture readonly %"));
    assert!(!call.contains(','), "{call}");
    for method in ["get_width(0u)", "get_height()", "get_num_mip_levels()"] {
        let bad = source.replace("get_width()", method);
        assert!(compile_source(&bad, &target, "t.metal").is_err(), "{bad}");
    }
    let read_write = source.replace(
        "texture_buffer<float>",
        "texture_buffer<float, access::read_write>",
    );
    assert!(compile_source(&read_write, &target, "t.metal").is_err());
    for target in [Target::macos(), Target::ios()] {
        assert!(compile_source(&source, &target, "t.metal")
            .is_err_and(|e| e.key() == "unsupported:texture_buffer:target"));
    }
}

#[test]
fn unsigned_texel_buffers_match_air28_read_and_write_operands() {
    let read_only = SOURCE
        .replace("texture_buffer<float>", "texture_buffer<uint>")
        .replace("fragment float4", "fragment uint4");
    let read_write = READ_WRITE
        .replace("texture_buffer<float", "texture_buffer<uint")
        .replace("float4 color", "uint4 color");
    let write_only = read_write
        .replace("access::read_write", "access::write")
        .replace("m.texels.read(lane)", "uint4(lane)");
    let target = Target::macos_for(26);
    for (source, read_mode, write_mode) in [
        (&read_only, Some(1), None),
        (&read_write, Some(3), Some(3)),
        (&write_only, None, Some(2)),
    ] {
        let ir = compile_source(source, &target, "t.metal").expect("emits");
        let read = ir
            .lines()
            .find(|l| l.contains("call { <4 x i32>, i8 } @air.read_texture_buffer_1d.u.v4i32("));
        assert_eq!(read.is_some(), read_mode.is_some(), "{ir}");
        if let (Some(call), Some(mode)) = (read, read_mode) {
            assert!(call.contains("nocapture readonly"), "{call}");
            assert!(call.contains("%struct._sampler_t addrspace(2)*"), "{call}");
            assert_eq!(call.matches("i32 ").count(), 2, "{call}");
            assert!(call.contains(&format!(", i32 {mode}) #")), "{call}");
        }
        let write = ir
            .lines()
            .find(|l| l.contains("call void @air.write_texture_buffer_1d.u.v4i32("));
        assert_eq!(write.is_some(), write_mode.is_some(), "{ir}");
        if let (Some(call), Some(mode)) = (write, write_mode) {
            assert!(!call.contains("sampler"), "{call}");
            assert_eq!(call.matches("i32 ").count(), 2, "{call}");
            assert!(call.contains(", <4 x i32> %"), "{call}");
            assert!(call.contains(&format!(", i32 {mode}) #")), "{call}");
        }
    }
    for source in [
        read_only.replace("uint(p.x)", "int(p.x)"),
        read_only.replace(
            "texture_buffer<uint>",
            "texture_buffer<uint, access::write>",
        ),
        read_write.replace(", access::read_write", ""),
        write_only.replace("color, lane", "color.x, lane"),
        write_only.replace("lane + 1u", "lane, 0u"),
    ] {
        assert!(
            compile_source(&source, &target, "t.metal").is_err(),
            "{source}"
        );
    }
    for target in [Target::macos(), Target::ios()] {
        assert!(compile_source(&write_only, &target, "t.metal")
            .is_err_and(|e| e.key() == "unsupported:texture_buffer:target"));
    }
}
