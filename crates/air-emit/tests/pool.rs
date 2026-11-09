//! Texture pools on synthetic MSL (no game data): `reinterpret_cast`, `array<T, N>` members of
//! argument buffers and the translator's `dm_pool_mark` helper. The calls, metadata and names follow
//! Apple's `metal` for the same source. Apple's IR also wraps arrays and atomics in the struct types
//! `metal::array` and `metal::_atomic`, the emitter uses the plain element types.

use air_emit::{compile_source, Target};

const PRE: &str = "#include <metal_stdlib>\nusing namespace metal;\n";

/// The helper text of `nxb-xlate`'s `pool::MARK`: the prelude finds it by the hash of its tokens.
const MARK: &str = "template<typename P> static inline __attribute__((always_inline)) uint dm_pool_mark(constant P* p, uint i)
{
    device atomic_uint* w = reinterpret_cast<device atomic_uint*>(p->seen) + (i >> 5);
    uint b = 1u << (i & 31u);
    if (!(atomic_load_explicit(w, memory_order_relaxed) & b)) atomic_fetch_or_explicit(w, b, memory_order_relaxed);
    return i;
}
";

const POOL: &str = "struct spvPool_t { uint n; ulong seen; array<texture2d<float>, 2> e; };
struct spvPool_s { uint n; ulong seen; array<sampler, 1> e; };
struct spvDescriptorSetBuffer0 { constant spvPool_t* t [[id(0)]]; constant spvPool_s* s [[id(1)]]; };
struct main0_out { float4 c [[color(0)]]; };
struct main0_in { float2 uv [[user(locn0)]]; };
";

fn emit(body: &str) -> String {
    compile_source(&format!("{PRE}{body}"), &Target::macos(), "t.metal").expect("emits")
}

fn pool_shader() -> String {
    emit(&format!(
        "{POOL}{MARK}fragment main0_out main0(main0_in in [[stage_in]], constant spvDescriptorSetBuffer0& d [[buffer(0)]], uint k [[sample_id]])
{{
    main0_out out = {{}};
    out.c = (&d.t->e[0])[dm_pool_mark(d.t, min(k, d.t->n))].sample((&d.s->e[0])[min(k, d.s->n)], in.uv);
    return out;
}}
"
    ))
}

#[test]
fn reinterpret_cast_of_an_address_and_of_a_pointer() {
    let ir = emit(
        "kernel void k(device float* p [[buffer(0)]], constant ulong& a [[buffer(1)]])
{
    device uint* q = reinterpret_cast<device uint*>(p);
    device uint* r = reinterpret_cast<device uint*>(a);
    q[0] = r[1];
}
",
    );
    assert!(ir.contains("bitcast float addrspace(1)* "), "{ir}");
    assert!(ir.contains(" to i32 addrspace(1)*"), "{ir}");
    assert!(ir.contains("inttoptr i64 "), "{ir}");
}

#[test]
fn pool_mark_is_one_instantiation_per_pool_type() {
    let ir = pool_shader();
    let def = "define internal i32 @_ZL12dm_pool_markI9spvPool_tEjPU11MTLconstantKT_j(%struct.spvPool_t addrspace(2)* noundef %a0, i32 noundef %a1)";
    assert!(ir.contains(def), "{ir}");
    assert_eq!(
        ir.matches("define internal i32 @_ZL12dm_pool_mark").count(),
        1
    );
    // the bit word: the address in `seen` plus i >> 5 words
    assert!(ir.contains("inttoptr i64 "));
    assert!(ir.contains("lshr i32 "));
    // relaxed order (0), device scope (2), not volatile
    assert!(ir.contains("call i32 @air.atomic.global.load.i32(i32 addrspace(1)* nocapture %"));
    assert!(ir.contains("call i32 @air.atomic.global.or.u.i32(i32 addrspace(1)* nocapture %"));
    assert!(ir.contains(", i32 0, i32 2, i32 0, i1 false)"));
    assert!(ir.contains(
        "declare i32 @air.atomic.global.load.i32(i32 addrspace(1)* nocapture, i32, i32, i32, i1)"
    ));
    assert!(ir.contains(
        "declare i32 @air.atomic.global.or.u.i32(i32 addrspace(1)* nocapture, i32, i32, i32, i32, i1)"
    ));
}

#[test]
fn the_pool_index_is_computed_before_the_sampler() {
    // clang evaluates the texture object (with its dm_pool_mark index) before the method's arguments
    let ir = pool_shader();
    let mark = ir.find("call i32 @_ZL12dm_pool_mark").expect("mark call");
    let sampler = ir
        .find(
            "load %struct._sampler_t addrspace(2)*, %struct._sampler_t addrspace(2)* addrspace(2)*",
        )
        .expect("sampler load");
    let sample = ir
        .find("call { <4 x float>, i8 } @air.sample_texture_2d.v4f32(")
        .expect("sample");
    assert!(mark < sampler && sampler < sample, "{ir}");
}

#[test]
fn metal_array_members_of_an_argument_buffer() {
    let ir = pool_shader();
    assert!(
        ir.contains("%struct.spvPool_t = type { i32, i64, [2 x %\"struct.metal::texture2d\"] }"),
        "{ir}"
    );
    assert!(
        ir.contains("%struct.spvPool_s = type { i32, i64, [1 x %\"struct.metal::sampler\"] }"),
        "{ir}"
    );
    assert!(ir.contains("!\"air.indirect_buffer\""));
    assert!(ir.contains(
        "!\"air.texture\", !\"air.location_index\", i32 2, i32 2, !\"air.sample\", !\"air.arg_type_name\", !\"array<texture2d<float, sample>, 2>\", !\"air.arg_name\", !\"e\""
    ), "{ir}");
    assert!(ir.contains(
        "!\"air.sampler\", !\"air.location_index\", i32 2, i32 1, !\"air.arg_type_name\", !\"array<sampler, 1>\", !\"air.arg_name\", !\"e\""
    ), "{ir}");
    assert!(ir.contains("!\"_ZTSN5metal5arrayINS_9texture2dIfLNS_6accessE0ELNS_16memory_coherenceE0EvEELm2EEE\""), "{ir}");
}

#[test]
fn a_metal_array_outside_a_struct_is_unsupported() {
    let e = compile_source(
        &format!("{PRE}float f(array<sampler, 2> s) {{ return 0.0; }}\n"),
        &Target::macos(),
        "t",
    )
    .expect_err("rejects");
    assert!(e.key().contains("metal-array"), "{}", e.key());
}
