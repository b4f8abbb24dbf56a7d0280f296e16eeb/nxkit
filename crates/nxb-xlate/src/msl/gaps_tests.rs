//! Tests of the gap closing features: each builds a small structured program and checks the MSL and the reflection.
//! The ignored test compiles the samples with Apple's compiler: `cargo test -p nxb-xlate -- --ignored gaps_metal`.

use super::emit::Options;
use super::library::build;
use super::tests::{B, compute};
use super::{Context, Translated};
use crate::attr::Stage;
use crate::driver::BufferInfo;
use crate::ir::{Inst, IoVariable, StorageKind};
use crate::props::{BufferDef, Field, MemoryDef};
use crate::structured::ast::{A, BlockType};
use crate::structured::types::AggType;
use crate::types::SamplerType;

pub(super) fn translate(b: &B) -> Translated {
    let ctx = Context {
        chain: "V",
        source: "p#0".into(),
        ..Context::default()
    };
    build(&b.s, &ctx, &Options::default()).expect("build")
}

pub(super) fn msl(b: &B) -> String {
    translate(b).libraries.remove(0).msl
}

/// The compute sample with a 48 KB shared array next to its small one.
fn big_shared() -> B {
    let mut b = compute();
    b.s.props.properties.shared_memories.push(MemoryDef {
        id: 1,
        name: "big".into(),
        ty: AggType(AggType::U32.0 | AggType::ARRAY.0),
        array_length: 12288,
        zero_init: false,
    });
    b
}

fn big_shared_with_workgroup_id() -> B {
    let mut b = big_shared();
    b.io(
        crate::ir::StorageKind::Input,
        crate::ir::IoVariable::CtaId,
        0,
    );
    b.load_io(
        crate::ir::StorageKind::Input,
        crate::ir::IoVariable::CtaId,
        None,
        0,
        AggType::U32,
    );
    b
}

#[test]
fn shared_memory_past_the_limit_spills_to_a_device_buffer() {
    let t = translate(&big_shared());
    let l = &t.libraries[0];
    for want in [
        "device uchar* dm_tg [[buffer(30)]]",
        "uint3 dm_ntg [[threadgroups_per_grid]]",
        "uint3 dm_tgid [[threadgroup_position_in_grid]]",
        "ulong dm_tgbase = ulong(((dm_tgid.z * dm_ntg.y + dm_tgid.y) * dm_ntg.x + dm_tgid.x) % 512u) * 49184ul;",
        "device uint (&sm0)[8] = *(device uint (*)[8])(dm_tg + dm_tgbase + 0);",
        "device uint (&sm1)[12288] = *(device uint (*)[12288])(dm_tg + dm_tgbase + 32);",
        "threadgroup_barrier(mem_flags::mem_device | mem_flags::mem_threadgroup);",
        "reinterpret_cast<device atomic_uint*>",
    ] {
        assert!(l.msl.contains(want), "missing {want}\n{}", l.msl);
    }
    assert!(!l.msl.contains("threadgroup uint"), "{}", l.msl);
    let r = l.reflection.as_ref().expect("reflection");
    assert!(
        r.extra
            .iter()
            .any(|(k, v)| k == "tg_spill" && *v == nxb_pyjson::Value::Int(49184))
    );
}

#[test]
fn the_workgroup_id_parameter_is_reused_by_the_spill() {
    let m = msl(&big_shared_with_workgroup_id());
    assert!(m.contains("gl_WorkGroupID [[threadgroup_position_in_grid]]"));
    assert!(!m.contains("dm_tgid"), "{m}");
}

#[test]
fn shared_memory_within_the_limit_stays_in_the_threadgroup() {
    let t = translate(&compute());
    let l = &t.libraries[0];
    assert!(l.msl.contains("threadgroup uint sm0[8];"));
    assert!(!l.msl.contains("dm_tg"));
    let r = l.reflection.as_ref().expect("reflection");
    assert!(r.extra.iter().all(|(k, _)| k != "tg_spill"));
}

/// A compute shader of local size 8x1x1 with the storage buffer `fp_s0`, and no statements.
pub(super) fn bare_compute() -> B {
    let mut b = B::new(Stage::Compute, "cs");
    b.s.props.definitions.compute_local_size = [8, 1, 1];
    b.io(StorageKind::Input, IoVariable::ThreadId, 0);
    b.io(StorageKind::Input, IoVariable::CtaId, 0);
    b.s.props.properties.storage_buffers.push(BufferDef {
        layout: 1,
        set: 0,
        binding: 0,
        name: "fp_s0".into(),
        fields: vec![Field {
            ty: AggType(AggType::U32.0 | AggType::ARRAY.0),
            array_length: 0,
            name: "data".into(),
        }],
    });
    b.s.program.sbufs.push(BufferInfo {
        binding: 0,
        slot: 0,
        cb_slot: 0,
        cb_offset: i64::from(crate::msl::abi::C0_SSBO_CS / 4),
        flags: 1,
    });
    b
}

fn store_word(b: &mut B, word: i32, value: i32) -> A {
    let src = vec![b.k(0), b.k(0), b.k(word), b.k(value)];
    b.op(Inst::Store, StorageKind::StorageBuffer, 0, src)
}

/// `if (first invocation) { x[2] = 7; x[3] = 9; }` after the test, then an unguarded store.
fn first_store_sample(second_use: bool) -> B {
    let mut b = bare_compute();
    let u = AggType::U32;
    let wg = b.load_io(StorageKind::Input, IoVariable::CtaId, None, 0, u);
    let lid = b.load_io(StorageKind::Input, IoVariable::ThreadId, None, 0, u);
    let three = b.k(3);
    let sh = b.op(Inst::ShiftLeft, StorageKind::None, 0, vec![wg, three]);
    let c = b.def(AggType::S32, sh);
    let not = b.op(Inst::BitwiseNot, StorageKind::None, 0, vec![lid]);
    let d = b.def(AggType::S32, not);
    let one = b.k(1);
    let add = b.op(Inst::Add, StorageKind::None, 0, vec![d, one]);
    let e = b.def(AggType::S32, add);
    let ne = b.op(Inst::CompareNotEqual, StorageKind::None, 0, vec![e, c]);
    let f = b.def(AggType::BOOL, ne);
    let nf = b.op(Inst::LogicalNot, StorageKind::None, 0, vec![f]);
    let g = b.def(AggType::BOOL, nf);
    let blk = b.s.info.ast.new_block(BlockType::If, Some(g));
    for (w, v) in [(2, 7), (3, 9)] {
        let st = store_word(&mut b, w, v);
        b.s.info.ast.add(blk, st).expect("add");
    }
    b.put(blk);
    let zero = b.k(0);
    let st = b.op(
        Inst::Store,
        StorageKind::StorageBuffer,
        0,
        vec![zero, zero, zero, lid],
    );
    b.put(st);
    if second_use {
        let blk = b.s.info.ast.new_block(BlockType::If, Some(g));
        let st = store_word(&mut b, 5, 1);
        b.s.info.ast.add(blk, st).expect("add");
        b.put(blk);
    }
    b
}

#[test]
fn the_first_invocation_stores_move_to_the_host() {
    let t = translate(&first_store_sample(false));
    let l = &t.libraries[0];
    assert!(!l.msl.contains("_m0[2]"), "{}", l.msl);
    assert!(!l.msl.contains("_m0[3]"), "{}", l.msl);
    assert!(l.msl.contains("_m0[0] = v9;"), "{}", l.msl);
    let r = l.reflection.as_ref().expect("reflection");
    let (_, v) = r
        .extra
        .iter()
        .find(|(k, _)| k == "first_store")
        .expect("first_store");
    let word = |w: i64, x: i64| {
        nxb_pyjson::Value::obj([
            ("reg", nxb_pyjson::Value::Int(0)),
            ("word", nxb_pyjson::Value::Int(w)),
            ("value", nxb_pyjson::Value::Int(x)),
        ])
    };
    assert_eq!(*v, nxb_pyjson::Value::List(vec![word(2, 7), word(3, 9)]));
}

#[test]
fn another_use_of_the_test_keeps_the_shader_as_it_is() {
    let t = translate(&first_store_sample(true));
    let l = &t.libraries[0];
    assert!(l.msl.contains("_m0[2]"), "{}", l.msl);
    let r = l.reflection.as_ref().expect("reflection");
    assert!(r.extra.iter().all(|(k, _)| k != "first_store"));
}

/// The compute sample with the HMMA helper the tensor pass makes: seventeen word arguments, one out argument and a
/// scratch shared memory it stores to.
fn hmma_sample() -> B {
    let mut b = compute();
    let main = b.s.info.ast.new_block(BlockType::Main, None);
    let (id, zero) = (b.k(0), b.k(0));
    let t = b.local(AggType::U32);
    let st = b.op(Inst::Store, StorageKind::SharedMemory, 0, vec![id, zero, t]);
    b.s.info.ast.add(main, st).expect("add");
    b.s.info.functions.push(crate::structured::StructFunction {
        main_block: main,
        name: "dmsh_hmma16816_f16".into(),
        return_type: AggType::U32,
        in_arguments: vec![AggType::U32; 17],
        out_arguments: vec![AggType::U32],
        locals: vec![],
    });
    b
}

#[test]
fn the_hmma_helper_runs_on_simdgroup_matrices() {
    let m = msl(&hmma_sample());
    for want in [
        "inline __attribute__((always_inline)) uint fn1(thread uint& a0, thread uint& a1,",
        "thread uint& a17, thread simdgroup_float8x8 (&acc)[16], thread simdgroup_half8x8 (&am)[4]",
        "threadgroup uint* s = &sm0[uint(a1)];",
        "uint g = uint(a0) >> 2, t = uint(a0) & 3;",
        "const int f = int(uint(a10)), k = (f >> 8) * 2;",
        "adirect = false;",
        "simdgroup_multiply_accumulate(c0, am[1], b1, c0);",
        "simdgroup_load(b0, h + 256, 16, ulong2(0, 0), true);",
        "a17 = as_type<uint>(d1);",
        "return as_type<uint>(d0);",
    ] {
        assert!(m.contains(want), "missing {want}\n{m}");
    }
}

/// A call outside every chain has no slot bits. It must not write slot 0, where a chain may hold its accumulator
/// (dotted rows in Ocarina of Time).
#[test]
fn the_hmma_helper_touches_a_slot_only_for_a_chain() {
    let m = msl(&hmma_sample());
    let body = &m[m.find("fn1(").expect("helper")..];
    let body = &body[..body.find("\n}\n").expect("end")];
    let writes: Vec<&str> = body.lines().filter(|l| l.contains("acc[k")).collect();
    assert_eq!(writes.len(), 2, "{body}");
    assert!(
        writes[0]
            .trim_start()
            .starts_with("else if (cslot) { simdgroup_multiply_accumulate(c0, am[0], b0, acc[k]);"),
        "{body}"
    );
    assert!(
        writes[1].trim_start().starts_with("if (dslot) {")
            && writes[1].contains("acc[k] = c0; acc[k + 1] = c1;"),
        "{body}"
    );
}

/// D is f16 in hardware after every HMMA, so a chain's accumulator slot is rounded to half before it is written, in the
/// registers (`thread_elements`) without a scratch round trip. The accumulator is still FP32 across both K = 8 halves.
#[test]
fn the_hmma_helper_rounds_a_slot_to_half() {
    let m = msl(&hmma_sample());
    let body = &m[m.find("fn1(").expect("helper")..];
    let body = &body[..body.find("\n}\n").expect("end")];
    let line = body
        .lines()
        .find(|l| l.contains("if (dslot)"))
        .expect("dslot");
    let round0 = line
        .find("c0.thread_elements()[1] = float(half(c0.thread_elements()[1]));")
        .expect("round c0");
    let round1 = line
        .find("c1.thread_elements()[1] = float(half(c1.thread_elements()[1]));")
        .expect("round c1");
    let store = line.find("acc[k] = c0;").expect("store");
    assert!(round0 < round1 && round1 < store, "{line}");
    // the rounding comes after both K = 8 products and adds no memory traffic or barrier before the slot store
    let sum = body
        .find("simdgroup_multiply_accumulate(c0, am[1], b1, c0);")
        .expect("second half");
    assert!(sum < body.find(line).unwrap(), "{body}");
    assert_eq!(line.matches("simdgroup_barrier").count(), 1, "{line}");
    assert_eq!(body.matches("thread_elements").count(), 8, "{body}");
}

/// The compute sample that calls the shared store helper `name` for `sm0` with a byte offset and a value.
fn shared_store_sample(name: &str, big: bool) -> B {
    let mut b = if big { big_shared() } else { compute() };
    let helper = b.s.info.ast.new_block(BlockType::Main, None);
    b.s.info.functions.push(crate::structured::StructFunction {
        main_block: helper,
        name: name.into(),
        return_type: AggType::VOID,
        in_arguments: vec![AggType::U32; 2],
        out_arguments: vec![],
        locals: vec![],
    });
    let src = vec![b.k(1), b.k(6), b.k(7)];
    let call = b.op(Inst::Call, StorageKind::None, 0, src);
    b.put(call);
    b
}

#[test]
fn a_byte_store_to_shared_memory_is_one_metal_store() {
    let m = msl(&shared_store_sample("SharedStore8_0", false));
    assert!(m.contains("fn1(t2, t3, "), "{m}");
    let body = &m[m.find("void fn1(").expect("helper")..];
    let body = &body[..body.find("\n}\n").expect("end")];
    assert!(
        body.contains("((threadgroup uchar*)&sm0[0])[uint(a0)] = uchar(uint(a1));"),
        "{body}"
    );
    assert!(!body.contains("atomic"), "{body}");
    assert!(!body.contains("while"), "{body}");
}

#[test]
fn a_half_word_store_to_shared_memory_is_one_aligned_may_alias_store() {
    let m = msl(&shared_store_sample("SharedStore16_0", false));
    let body = &m[m.find("void fn1(").expect("helper")..];
    let body = &body[..body.find("\n}\n").expect("end")];
    assert!(
        body.contains("typedef ushort __attribute__((may_alias)) dm_ushort_alias;"),
        "{body}"
    );
    assert!(
        body.contains(
            "((threadgroup dm_ushort_alias*)&sm0[0])[uint(a0) >> 1u] = ushort(uint(a1));"
        ),
        "{body}"
    );
}

#[test]
fn a_shared_store_of_a_spilled_kernel_is_a_device_store() {
    let m = msl(&shared_store_sample("SharedStore8_0", true));
    let body = &m[m.find("void fn1(").expect("helper")..];
    let body = &body[..body.find("\n}\n").expect("end")];
    assert!(
        body.contains("((device uchar*)&sm0[0])[uint(a0)] = uchar(uint(a1));"),
        "{body}"
    );
}

/// The helper accumulates in FP32 and rounds D to half once, as Ampere does. Apple's half `simdgroup_matrix` product
/// rounds after each step and overflows to infinity on a partial sum (speckles in Ocarina of
/// Time on M1 and M2).
#[test]
fn the_hmma_helper_accumulates_in_fp32() {
    let m = msl(&hmma_sample());
    let body = &m[m.find("fn1(").expect("helper")..];
    let body = &body[..body.find("\n}\n").expect("end")];
    for want in [
        "simdgroup_half8x8 b0, b1;",
        "simdgroup_float8x8 c0, c1;",
        "simdgroup_half8x8 ch0, ch1;",
        "simdgroup_multiply_accumulate(c0, am[0], b0, ch0);",
        "threadgroup float* w = (threadgroup float*)(s + 128);",
        "simdgroup_store(c0, w, 8, ulong2(0, 0)); simdgroup_store(c1, w, 8, ulong2(0, 8));",
        "uint d0 = as_type<uint>(half2(w[g * 8 + t * 2], w[g * 8 + t * 2 + 1]));",
        "uint d1 = as_type<uint>(half2(w[(g + 8) * 8 + t * 2], w[(g + 8) * 8 + t * 2 + 1]));",
    ] {
        assert!(body.contains(want), "missing {want}\n{body}");
    }
    assert!(
        !body.contains("simdgroup_half8x8 b0, b1, c0, c1;"),
        "{body}"
    );
}

fn bind_1d_level() -> B {
    super::bind_tests::sample(SamplerType::TEXTURE_1D, 1 << 5, |_, u, v| vec![u, v])
}

fn bind_off_dyn() -> B {
    super::bind_tests::sample(SamplerType::TEXTURE_2D, 1 << 6, |_, u, v| vec![u, v, u, v])
}

fn bind_off_dyn_level() -> B {
    super::bind_tests::sample(SamplerType::TEXTURE_2D, (1 << 6) | (1 << 5), |b, u, v| {
        vec![u, v, b.k(0), u, v]
    })
}

fn bind_3d_array() -> B {
    let ty = SamplerType(SamplerType::TEXTURE_3D.0 | SamplerType::ARRAY);
    super::bind_tests::sample(ty, 0, |b, u, v| vec![u, v, u, b.k(1)])
}

fn bind_cube_array() -> B {
    let ty = SamplerType(SamplerType::TEXTURE_CUBE.0 | SamplerType::ARRAY);
    super::bind_tests::image(ty, 4)
}

/// The samples compile with Apple's compiler.
#[test]
#[ignore]
fn gaps_metal_compiles_the_samples() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scratch");
    std::fs::create_dir_all(&dir).expect("scratch");
    let samples = [
        ("cs_spill", big_shared()),
        ("cs_spill_wg", big_shared_with_workgroup_id()),
        ("cs_first_store", first_store_sample(false)),
        ("cs_hmma", hmma_sample()),
        ("cs_sts8", shared_store_sample("SharedStore8_0", false)),
        ("cs_sts16", shared_store_sample("SharedStore16_0", false)),
        ("cs_sts8_spill", shared_store_sample("SharedStore8_0", true)),
        ("cs_wide", super::wide_tests::wide_sample(&[0, 1, 2, 5], 0)),
        (
            "ps_pool",
            super::pool_tests::pool_sample(1 << 20, 0, 0x4000_0000, false, false),
        ),
        (
            "ps_pool_sep",
            super::pool_tests::pool_sample(64, 7, 0x2a, true, false),
        ),
        (
            "cs_img_or",
            super::bind_tests::image_atomic(SamplerType::TEXTURE_2D, 11, 6 << 16, 2),
        ),
        (
            "cs_img_cas",
            super::bind_tests::image_atomic(SamplerType::TEXTURE_BUFFER, 12, 9 << 16, 1),
        ),
        (
            "cs_img_arr",
            super::bind_tests::image_atomic(
                SamplerType(SamplerType::TEXTURE_2D.0 | SamplerType::ARRAY),
                11,
                1 << 16,
                3,
            ),
        ),
        ("cs_spin", super::loop_tests::spin(true)),
        ("cs_gmem", super::gmem_tests::raw_sample()),
        ("ps_ia", super::ioidx_tests::indexed_pixel(None, 2)),
        ("ps_ia_const", super::ioidx_tests::indexed_pixel(Some(2), 1)),
        ("vs_ia_oa", super::ioidx_tests::indexed_vertex(None, 3)),
        ("ps_1d_level", bind_1d_level()),
        ("ps_off_dyn", bind_off_dyn()),
        ("ps_off_dyn_level", bind_off_dyn_level()),
        ("ps_tex3d_array", bind_3d_array()),
        (
            "cs_img_cube",
            super::bind_tests::image(SamplerType::TEXTURE_CUBE, 3),
        ),
        ("cs_img_cube_array", bind_cube_array()),
    ];
    for (n, b) in samples {
        let src = dir.join(format!("gaps_{n}.metal"));
        let air = dir.join(format!("gaps_{n}.air"));
        let text = msl(&b);
        std::fs::write(&src, &text).expect("write");
        let o = std::process::Command::new("xcrun")
            .args([
                "-sdk",
                "macosx",
                "metal",
                "-c",
                "-O1",
                "-fpreserve-invariance",
                "-fwrapv",
                "-std=metal3.1",
            ])
            .arg(&src)
            .arg("-o")
            .arg(&air)
            .output()
            .expect("run metal");
        assert!(
            o.status.success(),
            "{n}: {}\n{text}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
}
