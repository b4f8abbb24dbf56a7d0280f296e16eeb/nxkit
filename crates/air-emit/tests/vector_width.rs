//! Apple emits zero vector width for a vector held behind a buffer pointer.

use air_emit::{compile_source, Target};

fn emit(body: &str) -> String {
    let source = format!("#include <metal_stdlib>\nusing namespace metal;\n{body}");
    compile_source(&source, &Target::macos(), "-").expect("emits")
}

fn entry_width(ir: &str, name: &str) -> u32 {
    let name = format!("@{name}(");
    let definition = ir
        .lines()
        .find(|line| line.starts_with("define ") && line.contains(&name))
        .expect("entry definition");
    let (group, _) = definition
        .rsplit_once(" #")
        .expect("entry attribute group")
        .1
        .split_once(' ')
        .expect("attribute group and terminator");
    let prefix = format!("attributes #{group} = ");
    let attributes = ir
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .expect("attribute group");
    attributes
        .split_once("\"min-legal-vector-width\"=\"")
        .expect("width attribute")
        .1
        .split_once('"')
        .expect("width value")
        .0
        .parse()
        .expect("integer width")
}

#[test]
fn vector_buffer_pointer_does_not_raise_legal_width() {
    for declaration in [
        "constant uint2 values[3] = {uint2(2u, 7u)};",
        "constant float3 values[3] = {float3(-0.0, 0.0, 0.0)};",
    ] {
        let ty = declaration.split_ascii_whitespace().nth(1).expect("type");
        let ir = emit(&format!(
            "{declaration}\n\
             kernel void k(device {ty}* out [[buffer(0)]], uint i [[thread_index_in_threadgroup]]) {{ out[i] = values[i]; }}"
        ));
        assert_eq!(entry_width(&ir, "k"), 0, "{ir}");
    }
}

#[test]
fn vector_values_still_set_legal_width() {
    let signature = emit("kernel void k(device float4* out [[buffer(0)]], uint3 i [[thread_position_in_grid]]) { out[i.x] = float4(1.0); }");
    assert_eq!(entry_width(&signature, "k"), 96, "{signature}");

    let call = emit("kernel void k(device float4* out [[buffer(0)]], uint i [[thread_index_in_threadgroup]]) { out[i].xy = sin(out[i].xy); }");
    assert_eq!(entry_width(&call, "k"), 64, "{call}");
}
