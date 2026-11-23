//! Lowering of `mulhi`, the simd builtins, `front_facing` and `calculate_*_lod` on synthetic MSL.
//! The expected lines are what `metal -c -O1 -fmetal-math-mode=relaxed -fpreserve-invariance -fwrapv` emits for the same source.

use air_emit::{compile_source, Target};

const PRE: &str = "#include <metal_stdlib>\nusing namespace metal;\n";

fn emit(body: &str) -> String {
    compile_source(&format!("{PRE}{body}"), &Target::macos(), "t.metal").expect("emits")
}

fn has(ir: &str, s: &str) {
    assert!(ir.contains(s), "missing `{s}` in\n{ir}");
}

#[test]
fn mulhi_signed_and_unsigned() {
    let ir = emit(
        "kernel void k(device uint* p [[buffer(0)]], device int* q [[buffer(1)]], device int4* v [[buffer(2)]])\n\
         { p[0] = mulhi(p[1], 613566757u); q[0] = mulhi(q[1], -7); v[0] = mulhi(v[1], v[2]); }\n",
    );
    has(&ir, "call i32 @air.mul_hi.u.i32(i32 ");
    has(&ir, ", i32 613566757) #3");
    has(&ir, "call i32 @air.mul_hi.s.i32(i32 ");
    has(&ir, "call <4 x i32> @air.mul_hi.s.v4i32(<4 x i32> ");
    has(&ir, "declare i32 @air.mul_hi.u.i32(i32, i32) #3");
    has(&ir, "attributes #3 = { nounwind readnone willreturn }");
}

#[test]
fn simd_shuffle_freezes_the_data_and_votes_are_convergent() {
    let ir = emit(
        "struct O { float4 c0 [[color(0)]]; };\nstruct I { float4 u [[user(locn0)]]; };\n\
         fragment O main0(I in [[stage_in]], uint l [[thread_index_in_simdgroup]])\n\
         {\n    O o = {};\n    ushort n = ushort(l ^ 1u);\n\
             float f = simd_shuffle(in.u.x, n);\n    int i = simd_shuffle(as_type<int>(in.u.y), n);\n\
             uint u = simd_shuffle(as_type<uint>(in.u.z), n);\n    float2 v = simd_shuffle(in.u.zw, n);\n\
             bool b = simd_any(in.u.x > 0.0) && simd_all(in.u.y > 0.0);\n\
             o.c0 = float4(f + v.x, float(i), float(u), b ? 1.0 : 0.0);\n    return o;\n}\n",
    );
    has(
        &ir,
        "define <{ <4 x float> }> @main0(<4 x float> %a0, i32 noundef %a1)",
    );
    has(
        &ir,
        "!\"air.thread_index_in_simdgroup\", !\"air.arg_type_name\", !\"uint\"",
    );
    for (ty, f) in [
        ("float", "nsz arcp afn float @air.simd_shuffle.f32(float"),
        ("i32", "i32 @air.simd_shuffle.s.i32(i32"),
        ("i32", "i32 @air.simd_shuffle.u.i32(i32"),
        (
            "<2 x float>",
            "nsz arcp afn <2 x float> @air.simd_shuffle.v2f32(<2 x float>",
        ),
    ] {
        // the data operand is a frozen value, the lane an i16
        let call = ir
            .lines()
            .find(|l| l.contains(f))
            .unwrap_or_else(|| panic!("missing {f} in\n{ir}"));
        let data = call[call.find(f).unwrap_or(0) + f.len()..]
            .split(',')
            .next()
            .unwrap_or("")
            .trim();
        has(&ir, &format!("{data} = freeze {ty} "));
        assert!(call.contains(", i16 ") && call.ends_with(") #12"), "{call}");
    }
    has(&ir, "call i1 @air.simd_any(i1 ");
    has(&ir, "call i1 @air.simd_all(i1 ");
    has(&ir, "declare i1 @air.simd_any(i1) #12");
    has(&ir, "attributes #12 = { convergent nounwind willreturn }");
    has(
        &ir,
        "attributes #11 = { convergent nobuiltin \"no-builtins\" }",
    );
    assert_eq!(ir.matches("attributes #11 ").count(), 1);
}

#[test]
fn front_facing_stage_in_field() {
    let ir = emit(
        "struct I { float4 p [[position]]; float4 u [[user(locn5)]]; bool ff [[front_facing]]; };\n\
         struct O { float4 c0 [[color(0)]]; };\n\
         fragment O main0(I in [[stage_in]]) { O o = {}; o.c0 = in.u + as_type<float>(in.ff ? -1 : 0); return o; }\n",
    );
    has(
        &ir,
        "define <{ <4 x float> }> @main0(<4 x float> %a0, <4 x float> %a1, i1 %a2)",
    );
    has(&ir, "!{i32 2, !\"air.front_facing\", !\"air.arg_type_name\", !\"bool\", !\"air.arg_name\", !\"ff\"}");
}

#[test]
fn calculate_lod_on_a_texture_array() {
    let ir = emit(
        "struct spvDescriptorSetBuffer0 { texture2d_array<float> t [[id(0)]]; sampler s [[id(1)]]; };\n\
         struct O { float4 c0 [[color(0)]]; };\nstruct I { float4 u [[user(locn0)]]; };\n\
         fragment O main0(I in [[stage_in]], constant spvDescriptorSetBuffer0& d [[buffer(0)]])\n\
         {\n    O o = {};\n    float l = d.t.calculate_clamped_lod(d.s, in.u.xy);\n\
             float m = d.t.calculate_unclamped_lod(d.s, in.u.zw);\n    o.c0 = float4(l, m, 0.0, 1.0);\n    return o;\n}\n",
    );
    has(&ir, "declare float @air.calculate_clamped_lod_texture_2d_array(%struct._texture_2d_array_t addrspace(1)* nocapture readonly, %struct._sampler_t addrspace(2)* nocapture readonly, <2 x float>, i32) #5");
    has(
        &ir,
        "declare float @air.calculate_unclamped_lod_texture_2d_array(",
    );
    has(
        &ir,
        "attributes #5 = { argmemonly convergent nounwind readonly willreturn }",
    );
}

#[test]
fn simd_builtins_need_the_right_types() {
    let e = compile_source(
        &format!("{PRE}kernel void k(device uint* p [[buffer(0)]]) {{ p[0] = simd_any(p[1]) ? 1u : 0u; }}\n"),
        &Target::macos(),
        "t",
    )
    .unwrap_err();
    assert!(e.key().contains("simd-vote-form"), "{}", e.key());
}

#[test]
fn ballot_mask_is_a_convergent_64_bit_vote() {
    let ir = emit(
        "kernel void k(device ulong* out [[buffer(0)]], uint lane [[thread_index_in_simdgroup]]) {
        out[lane] = ulong(simd_ballot((lane & 1u) != 0u));
    }",
    );
    has(&ir, "call i64 @air.simd_ballot.i64(i1 ");
    has(&ir, "declare i64 @air.simd_ballot.i64(i1) #12");
    has(&ir, "attributes #12 = { convergent nounwind willreturn }");
}

#[test]
fn ballot_mask_can_be_read_as_two_words() {
    let ir = emit(
        "kernel void k(device uint* out [[buffer(0)]], uint lane [[thread_index_in_simdgroup]]) {
        ulong mask = ulong(simd_ballot(lane == 0u));
        out[0] = uint(mask & 0xfffffffful);
        out[1] = uint(mask >> 32u);
    }",
    );
    has(&ir, "call i64 @air.simd_ballot.i64(i1 ");
    assert_eq!(ir.matches("trunc i64").count(), 2, "{ir}");
    has(&ir, "lshr i64");
    has(&ir, "zext i32 32 to i64");
}

#[test]
fn relaxed_threadgroup_word_operations_use_local_scope() {
    let ir = emit(
        "kernel void k(device uint* out [[buffer(0)]]) {
        threadgroup uint words[1];
        words[0] = 0x80000000u;
        threadgroup atomic_uint* w = reinterpret_cast<threadgroup atomic_uint*>(&words[0]);
        out[0] = atomic_fetch_add_explicit(w, 2u, memory_order_relaxed);
        out[1] = atomic_fetch_min_explicit(w, 3u, memory_order_relaxed);
        out[2] = atomic_fetch_max_explicit(w, 0x80000001u, memory_order_relaxed);
    }",
    );
    for op in ["add", "min", "max"] {
        let call = format!("call i32 @air.atomic.local.{op}.u.i32(i32 addrspace(3)* nocapture ");
        let line = ir
            .lines()
            .find(|line| line.contains(&call))
            .expect("atomic call");
        has(line, ", i32 0, i32 1, i32 0, i1 false)");
    }
}

#[test]
fn threadgroup_barrier_checks_the_reaching_entry_stage() {
    let barrier = "threadgroup_barrier(mem_flags::mem_threadgroup);";
    let helpers = format!("static void sync() {{ {barrier} }} static void relay() {{ sync(); }}");
    let vertex = |call| {
        format!(
            "{helpers} struct O {{ float4 p [[position]]; }}; \
             vertex O v(uint i [[vertex_id]]) {{ {call} O o; o.p = float4(float(i)); return o; }}"
        )
    };
    has(&emit(&vertex("")), "!air.vertex");
    for call in [barrier, "relay();"] {
        let source = format!("{PRE}{}", vertex(call));
        let err = compile_source(&source, &Target::macos(), "t").unwrap_err();
        has(&err.key(), "threadgroup-barrier:vertex");
    }
    let fragment = emit(&format!(
        "{helpers} fragment float4 f() {{ relay(); return float4(1.0); }}"
    ));
    has(&fragment, "call void @air.wg.barrier");
}

#[test]
fn threadgroup_index_has_kernel_unsigned_scalar_abi() {
    for ty in ["uint", "ushort"] {
        let ir = emit(&format!(
            "kernel void k({ty} lane [[thread_index_in_threadgroup]]) {{}}"
        ));
        has(&ir, "air.thread_index_in_threadgroup");
    }
    for decl in [
        "kernel void k(float lane [[thread_index_in_threadgroup]]) {}",
        "struct O { float4 p [[position]]; }; vertex O v(uint lane [[thread_index_in_threadgroup]]) { O o; o.p = float4(float(lane)); return o; }",
    ] {
        let e = compile_source(&format!("{PRE}{decl}"), &Target::macos(), "t").unwrap_err();
        has(&e.key(), "threadgroup-index-parameter");
    }
}

#[test]
fn threadgroup_atomic_cannot_relabel_a_device_pointer() {
    for (expr, reason) in [
        (
            "static_cast<threadgroup atomic_uint*>(out)",
            "threadgroup-atomic-cast",
        ),
        (
            "reinterpret_cast<threadgroup atomic_uint*>(static_cast<threadgroup uint*>(out))",
            "pointer-cast-address-space",
        ),
    ] {
        let source = format!(
            "{PRE}kernel void k(device uint* out [[buffer(0)]]) {{ \
             threadgroup atomic_uint* w = {expr}; out[0] = atomic_fetch_add_explicit(w, 1u, memory_order_relaxed); }}"
        );
        let e = compile_source(&source, &Target::macos(), "t").unwrap_err();
        has(&e.key(), reason);
    }
}

#[test]
fn ballot_and_threadgroup_atomics_reject_invalid_forms() {
    for body in [
        "out[0] = uint(ulong(simd_ballot(uint2(1u))));",
        "threadgroup atomic_uint* w = reinterpret_cast<threadgroup atomic_uint*>(out);",
        "threadgroup uint a[1]; threadgroup atomic_uint* w = reinterpret_cast<threadgroup atomic_uint*>(&a[0]); out[0] = *w;",
        "threadgroup uint a[1]; threadgroup atomic_uint* w = reinterpret_cast<threadgroup atomic_uint*>(&a[0]); atomic_fetch_add_explicit(w, 1u, memory_order_seq_cst);",
    ] {
        let source = format!("{PRE}kernel void k(device uint* out [[buffer(0)]]) {{ {body} }}");
        assert!(compile_source(&source, &Target::macos(), "t").is_err(), "{body}");
    }
}

#[test]
fn uint_ctz_lowers_with_existing_clz_and_one_operand_evaluation() {
    let ir = emit(
        "kernel void k(device uint* p [[buffer(0)]])
         { p[0] = min(ctz(uint(p[1])), 15u);
           p[2] = ctz(0u); }",
    );
    assert_eq!(ir.matches("load i32,").count(), 1, "{ir}");
    assert_eq!(ir.matches("call i32 @air.clz.i32").count(), 2, "{ir}");
    assert!(!ir.contains("@air.ctz"), "{ir}");
    has(&ir, "declare i32 @air.clz.i32(i32, i1)");
    has(&ir, "sub i32 32,");
    has(&ir, "sub i32 0, 1");
    has(&ir, "xor i32 0, -1");
}

#[test]
fn conditional_vector_field_store_uses_selected_address_and_one_rhs() {
    let ir = emit(
        "struct O { float4 a; float4 b; float4 c; };
         kernel void k(device float* p [[buffer(0)]], uint i [[thread_index_in_threadgroup]])
         { O out = {}; (i == 0u ? out.a : i == 1u ? out.b : out.c).w = p[0]; p[1] = out.c.w; }",
    );
    assert_eq!(ir.matches("phi <4 x float>*").count(), 2, "{ir}");
    assert_eq!(
        ir.matches("load float, float addrspace(1)*").count(),
        1,
        "{ir}"
    );
    let phi = ir
        .lines()
        .rfind(|l| l.contains("phi <4 x float>*"))
        .unwrap();
    let addr = phi.trim().split_once(" = ").unwrap().0;
    assert_eq!(
        ir.lines()
            .filter(|l| l.contains("store <4 x float>") && l.contains(addr))
            .count(),
        1,
        "{ir}"
    );
}

#[test]
fn conditional_field_lvalue_does_not_admit_unrelated_forms() {
    for (statement, key) in [
        (
            "(i == 0u ? out.a : float4(0.0f)).w = 1.0f;",
            "assign-to-rvalue",
        ),
        (
            "(i == 0u ? out.a : out.b).w += 1.0f;",
            "conditional-field-compound-assignment",
        ),
        ("(i == 0u ? (*p).a : (*p).b).w = 1.0f;", "assign-to-rvalue"),
    ] {
        let source = format!(
            "{PRE}struct O {{ float4 a; float4 b; }}; kernel void k(device O* p [[buffer(0)]], uint i [[thread_index_in_threadgroup]]) {{ O out = {{}}; {statement} }}"
        );
        assert_eq!(
            compile_source(&source, &Target::macos(), "t.metal")
                .unwrap_err()
                .key(),
            format!("unsupported:rejected:{key}"),
            "{statement}"
        );
    }
}

#[test]
fn shared_halfword_alias_store_is_one_untagged_aligned_write() {
    let ir = emit(
        "void half_store(threadgroup uint (&words)[4], uint byte_offset, uint bits)
         { typedef ushort __attribute__((may_alias)) dm_ushort_alias;
           ((threadgroup dm_ushort_alias*)&words[0])[byte_offset >> 1u] = ushort(bits); }
         kernel void k(device uint* dst [[buffer(0)]], uint byte_offset [[thread_index_in_threadgroup]])
         { threadgroup uint words[4]; words[0] = 0x12345678u;
           half_store(words, byte_offset, dst[0]); dst[1] = words[0]; }",
    );
    let half: Vec<_> = ir
        .lines()
        .filter(|line| line.contains("store i16 "))
        .collect();
    assert_eq!(half.len(), 1, "{ir}");
    assert!(
        half[0].contains("i16 addrspace(3)*") && half[0].ends_with("align 2"),
        "{ir}"
    );
    assert!(!ir.contains("store i8 "), "{ir}");
    has(&ir, "getelementptr inbounds i16, i16 addrspace(3)*");
    assert!(
        ir.lines()
            .any(|l| l.contains("store i32 305419896, i32 addrspace(3)*") && l.contains("!tbaa")),
        "{ir}"
    );
}

#[test]
fn shared_halfword_alias_does_not_accept_device_or_plain_aliases() {
    for replacement in [
        ("may_alias", "unused"),
        ("threadgroup dm_ushort_alias*", "device dm_ushort_alias*"),
        ("[0u] = ushort(1u)", "[0u] = 1u"),
    ] {
        let src = format!(
            "{PRE}kernel void k() {{ threadgroup uint words[4];
              typedef ushort __attribute__((may_alias)) dm_ushort_alias;
              ((threadgroup dm_ushort_alias*)&words[0])[0u] = ushort(1u); }}"
        )
        .replace(replacement.0, replacement.1);
        assert!(
            compile_source(&src, &Target::macos(), "bad.metal").is_err(),
            "{src}"
        );
    }
}

#[test]
fn simd_is_helper_thread_uses_verified_target_signature() {
    let source = format!(
        "{PRE}fragment float4 f() {{ return float4(simd_is_helper_thread() ? 1.0 : 0.0); }}"
    );
    let ir = compile_source(&source, &Target::macos_for(26), "t.metal").expect("emits");
    has(&ir, "call i1 @air.simd_is_helper_thread() #12");
    has(&ir, "declare i1 @air.simd_is_helper_thread() #12");
    has(&ir, "attributes #12 = { convergent nounwind willreturn }");
    for target in [Target::ios(), Target::macos_for(27)] {
        let error = compile_source(&source, &target, "t.metal").unwrap_err();
        has(&error.key(), "simd-helper-thread:target");
    }

    let bad = source.replace("simd_is_helper_thread()", "simd_is_helper_thread(0u)");
    let error = compile_source(&bad, &Target::macos_for(26), "t.metal").unwrap_err();
    has(&error.key(), "simd-helper-thread-form");
}

#[test]
fn simdgroup_barrier_uses_verified_target_signature() {
    let source =
        format!("{PRE}kernel void k() {{ simdgroup_barrier(mem_flags::mem_threadgroup); }}");
    for (target, operands, types) in [
        (Target::macos_for(26), "i32 2, i32 4", "i32, i32"),
        (
            Target::macos_for(27),
            "i32 2, i32 5, i32 4",
            "i32, i32, i32",
        ),
    ] {
        let ir = compile_source(&source, &target, "t.metal").expect("emits");
        has(
            &ir,
            &format!("call void @air.simdgroup.barrier({operands}) #12"),
        );
        has(
            &ir,
            &format!("declare void @air.simdgroup.barrier({types}) #12"),
        );
        has(&ir, "attributes #12 = { convergent nounwind willreturn }");
        assert!(!ir.contains("air.wg.barrier"));
    }
    let error = compile_source(&source, &Target::ios(), "t.metal").unwrap_err();
    has(&error.key(), "simdgroup-barrier:target");
}

#[test]
fn simdgroup_barrier_requires_compute_and_one_known_mask() {
    let barrier = "simdgroup_barrier(mem_flags::mem_threadgroup);";
    let source = format!(
        "{PRE}static void sync() {{ {barrier} }} static void relay() {{ sync(); }} \
         fragment float4 f() {{ relay(); return float4(1.0); }}"
    );
    let error = compile_source(&source, &Target::macos(), "t.metal").unwrap_err();
    has(&error.key(), "simdgroup-barrier:noncompute");

    let source = format!(
        "{PRE}struct O {{ float4 p [[position]]; }}; vertex O v() {{ {barrier} \
         O o; o.p = float4(1.0); return o; }}"
    );
    let error = compile_source(&source, &Target::macos(), "t.metal").unwrap_err();
    has(&error.key(), "simdgroup-barrier:noncompute");
    for mask in [
        "mem_flags::mem_device",
        "mem_flags::mem_threadgroup | mem_flags::mem_device",
    ] {
        let source = format!("{PRE}kernel void k() {{ simdgroup_barrier({mask}); }}");
        assert!(compile_source(&source, &Target::macos(), "t.metal").is_err());
    }
    let ir = emit(
        "static uint simdgroup_barrier(uint x) { return x + 1u; } \
         kernel void k(device uint *p [[buffer(0)]]) { p[0] = simdgroup_barrier(1u); }",
    );
    assert!(!ir.contains("air.simdgroup.barrier"));
}
