//! AIR constant storage uses a bare element for a singleton packed prefix.

use air_emit::{compile_source, Target};

fn emit(ty: &str, count: u32, values: &str) -> String {
    let source = format!(
        "#include <metal_stdlib>\nusing namespace metal;\n\
         constant {ty} values[{count}] = {{{values}}};\n\
         kernel void k(device {ty}* out [[buffer(0)]], uint i [[thread_index_in_threadgroup]]) \
         {{ out[i] = values[i]; }}"
    );
    compile_source(&source, &Target::macos(), "-").expect("constant array emits")
}

#[test]
fn a_single_initialized_element_is_a_bare_packed_prefix() {
    for (ty, count, values, element, zero_tail, align) in [
        ("uint", 11, "5u", "i32", 10, 4),
        ("float3", 10, "float3(2.0)", "<3 x float>", 9, 16),
    ] {
        let ir = emit(ty, count, values);
        let packed = format!("<{{ {element}, [{zero_tail} x {element}] }}>");
        let global = ir
            .lines()
            .find(|line| line.contains(" = internal addrspace(2) constant "))
            .expect("constant global");
        assert!(
            global.contains(&format!("constant {packed} <{{ {element} ")),
            "{global}"
        );
        assert!(
            global.ends_with(&format!(
                "[{zero_tail} x {element}] zeroinitializer }}>, align {align}"
            )),
            "{global}"
        );
        assert!(
            ir.contains(&format!("bitcast ({packed} addrspace(2)* ")),
            "{ir}"
        );
        assert!(
            ir.contains(&format!("to [{count} x {element}] addrspace(2)*)")),
            "{ir}"
        );
    }
}

#[test]
fn other_constant_storage_shapes_are_unchanged() {
    for (count, values, storage) in [
        (
            11,
            "5u, 7u",
            "<{ [2 x i32], [9 x i32] }> <{ [2 x i32] [i32 5, i32 7], [9 x i32] zeroinitializer }>",
        ),
        (11, "", "[11 x i32] zeroinitializer"),
        (2, "5u", "[2 x i32] [i32 5, i32 0]"),
    ] {
        let ir = emit("uint", count, values);
        assert!(ir.contains(&format!("constant {storage}, align 4")), "{ir}");
    }
}
