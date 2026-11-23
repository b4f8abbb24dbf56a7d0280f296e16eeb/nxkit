//! Apple Metal 32023.921, AIR 2.8: a write-only float 2D array image store.

use air_front::{build_raw, Target};

const SOURCE: &str = "#include <metal_stdlib>
using namespace metal;
struct Args { texture2d_array<float, access::write> image [[id(0)]]; };
kernel void k(constant Args& a [[buffer(0)]], uint i [[thread_position_in_grid]]) {
    a.image.write(float4(float(i)), uint2(i, 0u), i + 1u);
}";

#[test]
fn float_array_store_emits_slice_before_texel() {
    let target = Target::macos_for(26);
    let ir = air_emit::compile_source(SOURCE, &target, "t.metal").expect("emits");
    let payload = ir
        .find("@air.convert.f.f32.u.i32(")
        .expect("payload conversion");
    let coord = ir
        .find("insertelement <2 x i32>")
        .expect("coordinate construction");
    assert!(payload < coord, "{ir}");
    let signature = "void @air.write_texture_2d_array.v4f32";
    assert!(
        ir.contains("!\"air.write\", !\"air.arg_type_name\", !\"texture2d_array<float, write>\"")
    );
    let decl = ir
        .lines()
        .find(|l| l.contains(&format!("declare {signature}")))
        .expect("declaration");
    assert!(decl.contains(
        "(%struct._texture_2d_array_t addrspace(1)* nocapture, <2 x i32>, i32, <4 x float>, i32, i32)"
    ), "{decl}");
    let call = ir
        .lines()
        .find(|l| l.contains(&format!("call {signature}")))
        .expect("call");
    assert!(call.contains(", i32 0, i32 2) #"), "{call}");

    let lib = build_raw(SOURCE.as_bytes(), &target).expect("raw library");
    let parsed = air_lib::parse(&lib).expect("metallib");
    assert_eq!(parsed.functions[0].name(), b"k");
}

#[test]
fn array_store_does_not_move_payload_across_coordinate_side_effects() {
    let source = SOURCE.replace(
        "    a.image.write(float4(float(i)), uint2(i, 0u), i + 1u);",
        "    uint j = i;\n    a.image.write(float4(float(j)), uint2(j += 1u, 0u), i);",
    );
    let ir = air_emit::compile_source(&source, &Target::macos_for(26), "t.metal").expect("emits");
    let update = ir.find("add i32 ").expect("coordinate update");
    let payload = ir
        .find("@air.convert.f.f32.u.i32(")
        .expect("payload conversion");
    assert!(update < payload, "{ir}");
}

#[test]
fn array_store_rejects_other_shapes_and_targets() {
    let target = Target::macos_for(26);
    for input in [
        SOURCE.replace("access::write", "access::read_write"),
        SOURCE
            .replace("float, access", "uint, access")
            .replace("float4(float(i))", "uint4(i)"),
        SOURCE.replace("i + 1u);", "i + 1u, 0u);"),
        SOURCE.replace("i + 1u);", "int(i) + 1);"),
        SOURCE.replace("uint2(i, 0u)", "int2(int(i), 0)"),
        SOURCE.replace("float4(float(i))", "float(i)"),
    ] {
        let error =
            air_emit::compile_source(&input, &target, "t.metal").expect_err("unproved form");
        assert_eq!(error.key(), "unsupported:write:texture2d_array");
    }
    for target in [Target::ios(), Target::macos()] {
        let error = air_emit::compile_source(SOURCE, &target, "t.metal").expect_err("unproved AIR");
        assert_eq!(error.key(), "unsupported:write:texture2d_array");
    }
}
