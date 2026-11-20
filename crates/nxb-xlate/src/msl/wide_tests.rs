//! Tests of the wide loads.

use super::gaps_tests::{bare_compute, msl};
use super::tests::B;
use crate::driver::BufferInfo;
use crate::ir::agg::AggType;
use crate::ir::{Inst, IoVariable, StorageKind};
use crate::props::{BufferDef, Field};

const WORDS: AggType = AggType(AggType::U32.0 | AggType::ARRAY.0);

/// A compute shader that reads words `at` of storage buffer 0 through byte addresses `thread * 16 + 4 * word`, in
/// order, and writes their sum to buffer 1. Buffer 0 is written too when `rw` is 1.
pub(super) fn wide_sample(at: &[i32], rw: i64) -> B {
    let mut b = bare_compute();
    b.s.program.sbufs[0].flags = rw;
    b.s.props.properties.storage_buffers.push(BufferDef {
        layout: 1,
        set: 0,
        binding: 1,
        name: "fp_s1".into(),
        fields: vec![Field {
            ty: WORDS,
            array_length: 0,
            name: "data".into(),
        }],
    });
    b.s.program.sbufs.push(BufferInfo {
        binding: 1,
        slot: 1,
        cb_slot: 0,
        cb_offset: 0,
        flags: 1,
    });
    let t = b.load_io(
        StorageKind::Input,
        IoVariable::ThreadId,
        None,
        0,
        AggType::U32,
    );
    let four = b.k(4);
    let shl = b.op(Inst::ShiftLeft, StorageKind::None, 0, vec![t, four]);
    let addr = b.def(AggType::S32, shl);
    let (zero, two) = (b.k(0), b.k(2));
    let mut sum = b.k(0);
    for w in at {
        let k = b.k(4 * w);
        let add = b.op(Inst::Add, StorageKind::None, 0, vec![addr, k]);
        let a = b.def(AggType::S32, add);
        let sh = b.op(Inst::ShiftRightU32, StorageKind::None, 0, vec![a, two]);
        let idx = b.def(AggType::S32, sh);
        let ld = b.op(
            Inst::Load,
            StorageKind::StorageBuffer,
            0,
            vec![zero, zero, idx],
        );
        let v = b.def(AggType::U32, ld);
        let add = b.op(Inst::Add, StorageKind::None, 0, vec![sum, v]);
        sum = b.def(AggType::U32, add);
    }
    let one = b.k(1);
    let st = b.op(
        Inst::Store,
        StorageKind::StorageBuffer,
        0,
        vec![one, zero, t, sum],
    );
    b.put(st);
    b
}

fn reads(text: &str) -> usize {
    text.matches("(*spvDescriptorSet0.fp_s0)._m0[").count()
}

#[test]
fn neighbouring_words_are_read_whole() {
    let m = msl(&wide_sample(&[0, 1, 2, 5], 0));
    assert!(
        m.contains(
            "packed_uint3 dw0 = *(const device packed_uint3*)&(*spvDescriptorSet0.fp_s0)._m0["
        ),
        "{m}"
    );
    for lane in ["dw0.x", "dw0.y", "dw0.z"] {
        assert!(m.contains(lane), "{lane}\n{m}");
    }
    assert!(!m.contains("dw0.w"), "{m}");
    assert_eq!(reads(&m), 2, "one wide read and the lone word\n{m}");
}

#[test]
fn four_words_make_a_quad() {
    let m = msl(&wide_sample(&[0, 1, 2, 3], 0));
    assert!(m.contains("packed_uint4 dw0 ="), "{m}");
    assert_eq!(reads(&m), 1, "{m}");
}

#[test]
fn a_run_starts_at_the_start_of_its_quad() {
    let m = msl(&wide_sample(&[1, 2, 3, 4], 0));
    assert!(!m.contains("packed_uint"), "{m}");
    assert_eq!(reads(&m), 4, "{m}");
}

#[test]
fn two_quads_make_two_runs() {
    let m = msl(&wide_sample(&[0, 1, 4, 5], 0));
    assert!(m.contains("packed_uint2 dw0 ="), "{m}");
    assert!(m.contains("packed_uint2 dw1 ="), "{m}");
    assert_eq!(reads(&m), 2, "{m}");
}

#[test]
fn a_lone_word_stays_a_word() {
    let m = msl(&wide_sample(&[0, 2], 0));
    assert!(!m.contains("packed_uint"), "{m}");
}

#[test]
fn a_buffer_the_shader_may_write_stays_as_it_is() {
    let m = msl(&wide_sample(&[0, 1, 2, 3], 1));
    assert!(!m.contains("packed_uint"), "{m}");
}

#[test]
fn the_wide_read_comes_first_in_the_run() {
    let m = msl(&wide_sample(&[2, 1, 0], 0));
    assert!(!m.contains("packed_uint"), "{m}");
}
