//! Tests of untraced global memory: the pool table, the write log and the physical pointer accesses the raw path leaves.

use super::gaps_tests::{bare_compute, msl, translate};
use super::tests::B;
use crate::driver::BufferInfo;
use crate::ir::{Inst, IoVariable, StorageKind};
use crate::props::{BufferDef, Field};
use crate::structured::types::AggType;

const WORDS: AggType = AggType(AggType::U32.0 | AggType::ARRAY.0);

fn table(b: &mut B, binding: i64, offset: i64, flags: i64) {
    b.s.props.properties.storage_buffers.push(BufferDef {
        layout: 1,
        set: 0,
        binding: binding as i32,
        name: format!("fp_s{binding}"),
        fields: vec![Field {
            ty: WORDS,
            array_length: 0,
            name: "data".into(),
        }],
    });
    b.s.program.sbufs.push(BufferInfo {
        binding,
        slot: binding,
        cb_slot: i64::from(crate::msl::abi::GMEM_MAP_CB_SLOT),
        cb_offset: offset,
        flags,
    });
}

/// What the raw path leaves: a table read, a logged bit, then a load, a store and an atomic add through the address
/// `(lo, hi)`. Buffer 1 is the table and buffer 2 the log.
pub(super) fn raw_sample() -> B {
    let mut b = bare_compute();
    table(&mut b, 1, 0, 0);
    table(&mut b, 2, i64::from(crate::msl::abi::GMEM_LOG_CB_OFFSET), 1);
    let (zero, one, two) = (b.k(0), b.k(1), b.k(2));
    let t = b.load_io(
        StorageKind::Input,
        IoVariable::ThreadId,
        None,
        0,
        AggType::U32,
    );
    let hint = b.op(
        Inst::Load,
        StorageKind::StorageBuffer,
        0,
        vec![one, zero, t],
    );
    let lo = b.def(AggType::U32, hint);
    let hi = b.k(0);
    let bit = b.op(Inst::ShiftLeft, StorageKind::None, 0, vec![one, t]);
    let bit = b.def(AggType::U32, bit);
    let or = b.op(
        Inst::AtomicOr,
        StorageKind::StorageBuffer,
        0,
        vec![two, zero, zero, bit],
    );
    b.put(or);
    let ld = b.op(Inst::Load, StorageKind::GlobalMemory, 0, vec![lo, hi]);
    let v = b.def(AggType::U32, ld);
    let st = b.op(Inst::Store, StorageKind::GlobalMemory, 0, vec![lo, hi, v]);
    b.put(st);
    let add = b.op(
        Inst::AtomicAdd,
        StorageKind::GlobalMemory,
        0,
        vec![lo, hi, v],
    );
    let r = b.def(AggType::U32, add);
    let out = b.op(
        Inst::Store,
        StorageKind::StorageBuffer,
        0,
        vec![zero, zero, t, r],
    );
    b.put(out);
    b
}

#[test]
fn untraced_accesses_go_through_a_device_pointer() {
    let m = msl(&raw_sample());
    assert!(
        m.contains("(*reinterpret_cast<device uint*>(as_type<ulong>(uint2("),
        "{m}"
    );
    assert!(m.contains("atomic_fetch_add_explicit("), "{m}");
}

#[test]
fn the_write_log_is_updated_atomically() {
    let m = msl(&raw_sample());
    assert!(m.contains("atomic_fetch_or_explicit("), "{m}");
}

#[test]
fn the_table_and_the_log_are_argument_buffer_members() {
    let l = translate(&raw_sample()).libraries.remove(0);
    let r = l.reflection.expect("reflection");
    let kinds: Vec<&str> = r.bindings.iter().map(|b| b.kind.as_str()).collect();
    assert!(kinds.contains(&"gmem_map"), "{kinds:?}");
    assert!(kinds.contains(&"gmem_log"), "{kinds:?}");
}

#[test]
fn a_signed_shared_compare_and_swap_is_a_bit_exact_unsigned_one() {
    let mut b = super::tests::compute();
    b.io(StorageKind::Input, IoVariable::GlobalId, 0);
    let zero = b.k(0);
    let g = b.load_io(
        StorageKind::Input,
        IoVariable::GlobalId,
        None,
        1,
        AggType::U32,
    );
    let (cmp, new) = (b.k(-1), b.k(-2));
    let cas = b.op(
        Inst::AtomicCompareAndSwap,
        StorageKind::SharedMemory,
        0,
        vec![zero, g, cmp, new],
    );
    let _ = b.def(AggType::S32, cas);
    let m = msl(&b);
    assert!(m.contains("atomic_compare_exchange_weak_explicit("), "{m}");
    assert!(m.contains("uint t3 = 4294967295u; uint t3c = t3;"), "{m}");
    // a weak exchange that fails with the expected value in memory tries again
    assert!(
        m.contains("while (!atomic_compare_exchange_weak_explicit("),
        "{m}"
    );
    assert!(m.contains("&& t3 == t3c) {}"), "{m}");
    assert!(m.contains("gl_GlobalInvocationID.y"), "{m}");
}
