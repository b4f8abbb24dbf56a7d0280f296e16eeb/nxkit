//! Apple AIR 2.8 access selectors for 2D image reads.

use air_front::{build_raw, Target};

const SOURCE: &str = "#include <metal_stdlib>
using namespace metal;
struct Args { texture2d<float, access::read> image [[id(0)]]; };
kernel void k(constant Args& a [[buffer(0)]], device float4* dst [[buffer(1)]],
              uint i [[thread_position_in_grid]]) {
    dst[i] = a.image.read(uint2(i, 0u));
}";

fn check(source: &str, target: &Target, selector: u32, intrinsic: &str) {
    let ir = air_emit::compile_source(source, target, "t.metal").expect("emits");
    let call = ir
        .lines()
        .find(|l| l.contains("call ") && l.contains(intrinsic))
        .expect("2D read call");
    assert!(
        call.contains(&format!(", i32 0, i32 {selector}) #")),
        "{call}"
    );
    let raw = build_raw(source.as_bytes(), target).expect("raw library");
    let lib = air_lib::parse(&raw).expect("metallib");
    assert_eq!(lib.functions[0].name(), b"k");
}

#[test]
fn float_2d_image_reads_use_access_selectors_on_air28() {
    const FLOAT_READ: &str = "@air.read_texture_2d.v4f32(";
    let target = Target::macos_for(26);
    check(SOURCE, &target, 1, FLOAT_READ);
    check(
        &SOURCE.replace("access::read", "access::read_write"),
        &target,
        3,
        FLOAT_READ,
    );
    check(
        &SOURCE.replace("access::read", "access::sample"),
        &target,
        0,
        FLOAT_READ,
    );
    check(SOURCE, &Target::macos(), 0, FLOAT_READ);
}

#[test]
fn integer_2d_image_reads_use_air28_storage_selectors() {
    let target = Target::macos_for(26);
    let legacy = Target::macos();
    for (scalar, intrinsic) in [
        ("int", "@air.read_texture_2d.s.v4i32("),
        ("uint", "@air.read_texture_2d.u.v4i32("),
    ] {
        let read = SOURCE
            .replace("texture2d<float", &format!("texture2d<{scalar}"))
            .replace("device float4*", &format!("device {scalar}4*"));
        let read_write = read.replace("access::read", "access::read_write");
        check(&read, &target, 1, intrinsic);
        check(&read_write, &target, 3, intrinsic);
        check(&read, &legacy, 0, intrinsic);
        check(&read_write, &legacy, 0, intrinsic);
    }
}

#[test]
fn float_2d_array_read_uses_air28_selector_one() {
    const ARRAY_READ: &str = "@air.read_texture_2d_array.v4f32(";
    let target = Target::macos_for(26);
    let source = SOURCE
        .replace("texture2d<float", "texture2d_array<float")
        .replace("read(uint2(i, 0u))", "read(uint2(i, 0u), i + 1u)");
    check(&source, &target, 1, ARRAY_READ);
    let ir = air_emit::compile_source(&source, &target, "t.metal").expect("emits");
    let call = ir
        .lines()
        .find(|l| l.contains("call ") && l.contains(ARRAY_READ))
        .expect("2D array read");
    assert!(
        call.contains(", <2 x i32> zeroinitializer, i32 0, i32 1) #"),
        "{call}"
    );

    check(
        &source.replace("access::read", "access::sample"),
        &target,
        0,
        ARRAY_READ,
    );
    check(
        &source.replace("i + 1u);", "i + 1u, 0u);"),
        &target,
        0,
        ARRAY_READ,
    );
    check(&source, &Target::macos(), 0, ARRAY_READ);
}
