//! Global array initializers preserve typed values and reject inexact conversions.
use air_front::{build_raw, compile, Ir, Reject, Target};

fn source(ty: &str, extent: &str, values: &str) -> String {
    format!(
        "#include <metal_stdlib>\nusing namespace metal;\n\
         constant {ty} values[{extent}] = {{{values}}};\n\
         kernel void k(device {ty}* out [[buffer(0)]], uint i [[thread_index_in_threadgroup]]) \
         {{ out[i] = values[i]; }}"
    )
}

fn same(source: &str, reference: &str, target: &Target) {
    let got = compile(source.as_bytes(), target).expect(source);
    assert!(matches!(got, Ir::Bitcode(_)), "{source}");
    assert_eq!(
        got,
        compile(reference.as_bytes(), target).expect(reference),
        "{source}"
    );
    assert_eq!(
        build_raw(source.as_bytes(), target).expect(source),
        build_raw(reference.as_bytes(), target).expect(reference),
        "{source}"
    );
}

fn rejects(source: &str, reason: &str, target: &Target) {
    let reject = Reject(format!("unsupported:const-array:{reason}"));
    assert_eq!(
        compile(source.as_bytes(), target),
        Err(reject.clone()),
        "{source}"
    );
    assert_eq!(
        build_raw(source.as_bytes(), target),
        Err(Ok(reject)),
        "{source}"
    );
}

#[test]
fn out_of_type_literals_and_implicit_narrowing_reject() {
    let target = Target::macos();
    for (ty, extent, value, reason) in [
        ("uint", "11", "4294967296u", "integer-literal-range"),
        (
            "ulong",
            "11",
            "18446744073709551616ul",
            "integer-literal-range",
        ),
        ("uint", "", "4294967296u", "integer-literal-range"),
        ("uint", "11", "1.5f", "implicit-narrowing"),
        ("uint", "11", "1.0f", "implicit-narrowing"),
        ("bool", "11", "1.0f", "implicit-narrowing"),
        ("bool", "11", "2", "implicit-narrowing"),
        ("uint", "11", "-1", "implicit-narrowing"),
        ("ushort", "11", "65536", "implicit-narrowing"),
        ("ushort", "11", "uint(float(65536))", "implicit-narrowing"),
        (
            "ushort",
            "11",
            "uint(float(16777217u))",
            "implicit-narrowing",
        ),
        ("float", "11", "16777217u", "implicit-narrowing"),
        ("half", "11", "2049", "implicit-narrowing"),
    ] {
        let src = source(ty, extent, value);
        rejects(&src, reason, &target);
    }
}

#[test]
fn explicit_casts_and_representable_implicit_conversions_still_match() {
    let target = Target::macos();
    for (ty, value, expected) in [
        ("uint", "1", "1u"),
        ("uint", "uint(1.5f)", "1u"),
        ("uint", "ushort(7u)", "7u"),
        ("ulong", "4294967295u", "4294967295ul"),
        ("ushort", "65535", "ushort(65535u)"),
        ("ushort", "uint(float(ushort(2u)))", "ushort(2u)"),
        ("ushort", "uint(-0.0f)", "ushort(0u)"),
        ("bool", "1", "true"),
        ("bool", "bool(1.0f)", "true"),
        ("float", "16777216u", "16777216.0f"),
        ("float", "uint(9)", "9.0f"),
        ("float", "uint(1.5f)", "1.0f"),
        ("half", "2048", "2048.0h"),
    ] {
        let input = source(ty, "11", value);
        let reference = source(ty, "11", expected);
        same(&input, &reference, &target);
    }
}

#[test]
fn typed_integer_lanes_cannot_silently_wrap() {
    let target = Target::macos();
    for (ty, value) in [
        ("uint2", "uint2(4294967296u, 1u)"),
        ("uint2", "uint2(4294967296u)"),
        ("uint2", "uint2(0x100000000u, 1u)"),
        ("ulong2", "ulong2(18446744073709551616ul, 1ul)"),
    ] {
        let src = source(ty, "11", value);
        rejects(&src, "non-constant-init", &target);
    }
}

#[test]
fn integer_lane_limits_and_signed_negation_still_compile() {
    let target = Target::macos();
    for (ty, value) in [
        ("uint2", "uint2(4294967295u, 1u)"),
        ("ulong2", "ulong2(18446744073709551615ul, 1ul)"),
        ("int2", "int2(-1, 2147483647)"),
        ("bool2", "bool2(true, false)"),
    ] {
        let src = source(ty, "11", value);
        assert!(
            matches!(compile(src.as_bytes(), &target), Ok(Ir::Bitcode(_))),
            "{src}"
        );
        assert!(build_raw(src.as_bytes(), &target).is_ok(), "{src}");
    }
}

#[test]
fn nested_vector_casts_match_the_typed_values() {
    let target = Target::macos_for(27);
    for (ty, init, control) in [
        ("uint2", "uint2(float2(ushort2(1u, 2u)))", "uint2(1u, 2u)"),
        ("uint2", "uint2(ushort2(1u, 2u))", "uint2(1u, 2u)"),
        (
            "uint2",
            "uint2(ushort2(float2(1.5f, 2.5f)))",
            "uint2(1u, 2u)",
        ),
        (
            "float2",
            "float2(int2(float2(-1.5f, 4.25f)))",
            "float2(-1.0f, 4.0f)",
        ),
    ] {
        let got = source(ty, "3", init);
        let expected = source(ty, "3", control);
        same(&got, &expected, &target);
    }
}

#[test]
fn nested_vector_casts_still_decline_unmodeled_or_inexact_steps() {
    let target = Target::macos_for(27);
    for (ty, init) in [
        ("uint2", "uint2(ushort2(65537u, 65538u))"),
        ("uint2", "uint2(float2(uint2(16777217u, 1u)))"),
        ("uint2", "uint2(float2(short2(-1, -2)))"),
        ("float2", "float2(half2(0.5h, 1.0h))"),
        ("bool2", "bool2(float2(0.5f, -0.5f))"),
    ] {
        let src = source(ty, "3", init);
        rejects(&src, "non-constant-init", &target);
    }
}

#[test]
fn array_initializer_needs_an_explicit_vector_type_change() {
    let target = Target::macos_for(27);
    for (ty, init, explicit, expected) in [
        (
            "float2",
            "uint2(1u, 2u)",
            "float2(uint2(1u, 2u))",
            "float2(1.0f, 2.0f)",
        ),
        (
            "uint2",
            "float2(1.0f, 2.0f)",
            "uint2(float2(1.0f, 2.0f))",
            "uint2(1u, 2u)",
        ),
    ] {
        let implicit = source(ty, "3", init);
        rejects(&implicit, "implicit-narrowing", &target);
        same(
            &source(ty, "3", explicit),
            &source(ty, "3", expected),
            &target,
        );
    }
    same(
        &source("float2", "3", "2.0f"),
        &source("float2", "3", "float2(2.0f)"),
        &target,
    );
}
