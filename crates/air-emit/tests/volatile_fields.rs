//! Read-only volatile argument-buffer data, as emitted by the geometry capture translator.

use air_emit::{compile_source, Target};

const PRE: &str = "#include <metal_stdlib>
using namespace metal;
struct Src { float data[4]; float4 vector; };
struct Args {
    const device volatile Src* src [[id(0)]];
    device float* dst [[id(1)]];
};
kernel void main0(constant Args& a [[buffer(0)]], uint i [[thread_position_in_grid]]) {
";

fn emit(body: &str) -> Result<String, String> {
    compile_source(&format!("{PRE}{body}\n}}"), &Target::macos(), "-").map_err(|err| err.key())
}

#[test]
fn only_scalar_pointee_loads_are_volatile() {
    let ir = emit(
        "float first = (*a.src).data[i];
         float second = (*a.src).data[i];
         float unused = (*a.src).data[i];
         a.dst[i] = first + second;",
    )
    .expect("geometry capture loads");
    assert_eq!(
        ir.matches("load volatile float, float addrspace(1)*")
            .count(),
        3
    );
    assert_eq!(ir.matches("load volatile ").count(), 3);
    assert!(ir.contains("load %struct.Src addrspace(1)*,"));
    assert!(ir.contains("store float "));
    assert!(!ir.contains("store volatile "));
}

#[test]
fn volatile_fields_cannot_escape_or_be_written() {
    for (body, expected) in [
        (
            "device Src* p = a.src;",
            "unsupported:volatile-field:pointer-escape",
        ),
        (
            "(*a.src).data[i] = 1.0f;",
            "unsupported:volatile-field:write-or-escape",
        ),
        (
            "device float* p = &(*a.src).data[i];",
            "unsupported:volatile-field:write-or-escape",
        ),
        (
            "Src copy = *a.src;",
            "unsupported:volatile-field:non-scalar",
        ),
        (
            "a.dst[i] = (*a.src).vector.x;",
            "unsupported:volatile-field:non-scalar",
        ),
        (
            "a.dst[i] = (*a.src).vector[i];",
            "unsupported:volatile-field:non-scalar",
        ),
    ] {
        assert_eq!(emit(body).unwrap_err(), expected, "{body}");
    }
}

#[test]
fn volatile_fields_cannot_escape_by_reference() {
    let reference = PRE.replace(
        "kernel void",
        "void touch(thread float& r) { r = 0.0f; }\nkernel void",
    );
    let result = compile_source(
        &format!("{reference} touch((*a.src).data[i]); }}"),
        &Target::macos(),
        "-",
    );
    assert_eq!(
        result.unwrap_err().key(),
        "unsupported:volatile-field:write-or-escape"
    );
}

#[test]
fn volatile_type_alias_is_not_admitted() {
    let source = PRE
        .replace(
            "struct Args {",
            "typedef const device volatile Src* SrcPtr;\nstruct Args {",
        )
        .replace("const device volatile Src* src", "SrcPtr src");
    assert!(compile_source(
        &format!("{source} a.dst[i] = (*a.src).data[i]; }}"),
        &Target::macos(),
        "-"
    )
    .is_err());
}
