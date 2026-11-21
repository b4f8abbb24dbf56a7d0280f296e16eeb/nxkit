//! `GlobalToStorage` helper functions: the single target and multi target functions that perform a storage buffer
//! operation (small integers, atomics that cannot be inlined, addresses that may point to several storage buffers).
//! The raw form (address traced to no storage buffer, reads a memory pool table) stays fail-closed.
use super::Env;
use crate::helper::{B, Function};
use crate::ir::{Inst, Opd, StorageKind};
use crate::translation::host;

type R<T> = Result<T, String>;

/// `GtsContext.Entry`.
pub(super) struct Entry {
    pub function_id: i32,
    inst: Inst,
    kind: StorageKind,
    multi: bool,
    targets: Vec<u32>,
}

impl Entry {
    pub(super) fn new(
        function_id: i32,
        inst: Inst,
        kind: StorageKind,
        multi: bool,
        targets: Vec<u32>,
    ) -> Entry {
        Entry {
            function_id,
            inst,
            kind,
            multi,
            targets,
        }
    }
}

/// `GtsContext.TryGetFunctionId`.
pub(super) fn find_function(
    entries: &[Entry],
    inst: Inst,
    kind: StorageKind,
    multi: bool,
    targets: &[u32],
) -> Option<i32> {
    entries
        .iter()
        .find(|e| e.inst == inst && e.kind == kind && e.multi == multi && e.targets == targets)
        .map(|e| e.function_id)
}

pub(super) fn pack_cb(slot: i32, off: i32) -> u32 {
    ((slot as u16) as u32) | (((off as u16) as u32) << 16)
}

fn unpack_cb(p: u32) -> (i32, i32) {
    ((p as u16) as i32, ((p >> 16) as u16) as i32)
}

pub(super) fn is_atomic(inst: Inst) -> bool {
    matches!(
        inst,
        Inst::AtomicAdd
            | Inst::AtomicAnd
            | Inst::AtomicCompareAndSwap
            | Inst::AtomicMaxS32
            | Inst::AtomicMaxU32
            | Inst::AtomicMinS32
            | Inst::AtomicMinU32
            | Inst::AtomicOr
            | Inst::AtomicSwap
            | Inst::AtomicXor
    )
}

pub(super) fn is_small_int(k: StorageKind) -> bool {
    matches!(
        k,
        StorageKind::GlobalMemoryS8
            | StorageKind::GlobalMemoryS16
            | StorageKind::GlobalMemoryU8
            | StorageKind::GlobalMemoryU16
    )
}

pub(super) fn inst_name(inst: Inst) -> R<&'static str> {
    Ok(match inst {
        Inst::Load => "Load",
        Inst::Store => "Store",
        Inst::AtomicAdd => "AtomicAdd",
        Inst::AtomicAnd => "AtomicAnd",
        Inst::AtomicCompareAndSwap => "AtomicCompareAndSwap",
        Inst::AtomicMaxS32 => "AtomicMaxS32",
        Inst::AtomicMaxU32 => "AtomicMaxU32",
        Inst::AtomicMinS32 => "AtomicMinS32",
        Inst::AtomicMinU32 => "AtomicMinU32",
        Inst::AtomicOr => "AtomicOr",
        Inst::AtomicSwap => "AtomicSwap",
        Inst::AtomicXor => "AtomicXor",
        _ => return Err("unported: helper function name of this instruction".into()),
    })
}

/// `GetFunctionName`.
pub(super) fn function_name(
    inst: Inst,
    kind: StorageKind,
    multi: bool,
    targets: &[u32],
) -> R<String> {
    let mut name = inst_name(inst)?.to_string();
    name.push_str(match kind {
        StorageKind::GlobalMemoryS8 => "S8",
        StorageKind::GlobalMemoryS16 => "S16",
        StorageKind::GlobalMemoryU8 => "U8",
        StorageKind::GlobalMemoryU16 => "U16",
        _ => "",
    });
    if multi {
        name.push_str("Multi");
    }
    for t in targets {
        let (slot, off) = unpack_cb(*t);
        name.push_str(&format!("_c{slot}o{off}"));
    }
    Ok(name)
}

pub(super) fn bit_offset(b: &mut B, offset: Opd) -> Opd {
    let c3 = b.ir.const_(3);
    let m = b.op(Inst::BitwiseAnd, &[offset, c3]);
    let c3b = b.ir.const_(3);
    b.op(Inst::ShiftLeft, &[m, c3b])
}

/// `GenerateAtomicCasLoop` for a bitfield insert of `value`.
fn cas_loop_insert(
    b: &mut B,
    word_offset: Opd,
    binding: i32,
    value: Opd,
    bit_offset: Opd,
    bit_size: i32,
) -> Opd {
    let head = b.ir.label();
    b.mark_label(head);
    let (c_b, c0) = (b.ir.const_(binding), b.ir.const_(0));
    let old = b.op_kind(
        Inst::Load,
        StorageKind::StorageBuffer,
        &[c_b, c0, word_offset],
    );
    let size = b.ir.const_(bit_size);
    let new = b.op(Inst::BitfieldInsert, &[old, value, bit_offset, size]);
    let (c_b, c0) = (b.ir.const_(binding), b.ir.const_(0));
    let cas = b.op_kind(
        Inst::AtomicCompareAndSwap,
        StorageKind::StorageBuffer,
        &[c_b, c0, word_offset, old, new],
    );
    let fail = b.op(Inst::CompareNotEqual, &[cas, old]);
    b.branch_if_true(head, fail);
    old
}

/// `TryGenerateStorageOp`: `Ok(None)` when no storage buffer can be reserved, else the result value (if any).
#[allow(clippy::too_many_arguments)]
fn generate_storage_op(
    b: &mut B,
    env: &mut Env,
    inst: Inst,
    kind: StorageKind,
    offset: Opd,
    compare: Option<Opd>,
    value: Option<Opd>,
    slot: i32,
    off: i32,
) -> R<Option<Option<Opd>>> {
    let is_store = is_atomic(inst) || inst == Inst::Store;
    let Some(binding) = env
        .rm
        .try_get_storage_buffer_binding(env.acc, slot, off, is_store)
    else {
        return Ok(None);
    };
    let c2 = b.ir.const_(2);
    let word = b.op(Inst::ShiftRightU32, &[offset, c2]);
    let mut result = None;
    if is_atomic(inst) {
        if is_small_int(kind) {
            return Err("NotImplementedException".into());
        }
        let v = value.ok_or("atomic without value")?;
        let (c_b, c0) = (b.ir.const_(binding), b.ir.const_(0));
        let srcs: Vec<Opd> = if inst == Inst::AtomicCompareAndSwap {
            vec![c_b, c0, word, compare.ok_or("cas without compare")?, v]
        } else {
            vec![c_b, c0, word, v]
        };
        result = Some(b.op_kind(inst, StorageKind::StorageBuffer, &srcs));
    } else if inst == Inst::Store {
        let bit_size = match kind {
            StorageKind::GlobalMemoryS8 | StorageKind::GlobalMemoryU8 => 8,
            StorageKind::GlobalMemoryS16 | StorageKind::GlobalMemoryU16 => 16,
            _ => 32,
        };
        let v = value.ok_or("store without value")?;
        if bit_size < 32 {
            let bo = bit_offset(b, offset);
            cas_loop_insert(b, word, binding, v, bo, bit_size);
        } else {
            let (c_b, c0) = (b.ir.const_(binding), b.ir.const_(0));
            b.raw(
                Inst::Store,
                StorageKind::StorageBuffer,
                None,
                &[c_b, c0, word, v],
            );
        }
    } else {
        let (c_b, c0) = (b.ir.const_(binding), b.ir.const_(0));
        let mut v = b.op_kind(Inst::Load, StorageKind::StorageBuffer, &[c_b, c0, word]);
        if is_small_int(kind) {
            let bo = bit_offset(b, offset);
            match kind {
                StorageKind::GlobalMemoryS8 | StorageKind::GlobalMemoryS16 => {
                    v = b.op(Inst::ShiftRightS32, &[v, bo]);
                    let z = b.ir.const_(0);
                    let w = b.ir.const_(if kind == StorageKind::GlobalMemoryS8 {
                        8
                    } else {
                        16
                    });
                    v = b.op(Inst::BitfieldExtractS32, &[v, z, w]);
                }
                _ => {
                    v = b.op(Inst::ShiftRightU32, &[v, bo]);
                    let m = b.ir.const_(if kind == StorageKind::GlobalMemoryU8 {
                        0xff
                    } else {
                        0xffff
                    });
                    v = b.op(Inst::BitwiseAnd, &[v, m]);
                }
            }
        }
        result = Some(v);
    }
    Ok(Some(result))
}

pub(super) fn finish_function(b: B, name: &str, returns: bool, in_args: i32) -> R<Function> {
    b.finish_with(name, returns, in_args, 0)
}

/// `TryGenerateSingleTargetStorageOp` without the cache lookup: `Ok(None)` when no storage buffer can be reserved.
pub(super) fn single_target(
    ir: &mut crate::ir::Ir,
    env: &mut Env,
    inst: Inst,
    kind: StorageKind,
    slot: i32,
    off: i32,
) -> R<Option<Function>> {
    let in_args = if inst == Inst::AtomicCompareAndSwap {
        3
    } else if inst == Inst::Store || is_atomic(inst) {
        2
    } else {
        1
    };
    let mut b = B::new(ir);
    let offset = b.ir.argument(0);
    let (mut compare, mut value) = (None, None);
    if in_args == 3 {
        compare = Some(b.ir.argument(1));
        value = Some(b.ir.argument(2));
    } else if in_args == 2 {
        value = Some(b.ir.argument(1));
    }
    let Some(res) =
        generate_storage_op(&mut b, env, inst, kind, offset, compare, value, slot, off)?
    else {
        return Ok(None);
    };
    b.ret(res);
    let name = function_name(inst, kind, false, &[pack_cb(slot, off)])?;
    Ok(Some(finish_function(b, &name, res.is_some(), in_args)?))
}

/// The body of `TryGenerateMultiTargetStorageOp` once the targets are known (not empty).
pub(super) fn multi_target(
    ir: &mut crate::ir::Ir,
    env: &mut Env,
    inst: Inst,
    kind: StorageKind,
    returns_value: bool,
    targets: &[u32],
) -> R<Option<Function>> {
    let in_args = if inst == Inst::AtomicCompareAndSwap {
        4
    } else if inst == Inst::Store || is_atomic(inst) {
        3
    } else {
        2
    };
    let mut b = B::new(ir);
    let lo = b.ir.argument(0);
    let hi = b.ir.argument(1);
    for &t in targets {
        let (slot, off) = unpack_cb(t);
        let base_lo = b.ir.cbuf(slot, off);
        let base_hi = b.ir.cbuf(slot, off + 1);
        let size = b.ir.cbuf(slot, off + 2);
        let offset = b.op(Inst::Subtract, &[lo, base_lo]);
        let borrow = b.op(Inst::CompareLessU32, &[lo, base_lo]);
        let in_low = b.op(Inst::CompareLessU32, &[offset, size]);
        let hi_b = b.op(Inst::Add, &[hi, borrow]);
        let in_high = b.op(Inst::CompareEqual, &[hi_b, base_hi]);
        let in_range = b.op(Inst::BitwiseAnd, &[in_low, in_high]);
        let skip = b.ir.label();
        b.branch_if_false(skip, in_range);
        let (mut compare, mut value) = (None, None);
        if in_args == 4 {
            compare = Some(b.ir.argument(2));
            value = Some(b.ir.argument(3));
        } else if in_args == 3 {
            value = Some(b.ir.argument(2));
        }
        let mask = b.ir.const_(-host::STORAGE_BUFFER_OFFSET_ALIGNMENT);
        let masked = b.op(Inst::BitwiseAnd, &[base_lo, mask]);
        let host_offset = b.op(Inst::Subtract, &[lo, masked]);
        let Some(res) = generate_storage_op(
            &mut b,
            env,
            inst,
            kind,
            host_offset,
            compare,
            value,
            slot,
            off,
        )?
        else {
            return Ok(None);
        };
        b.ret(res);
        b.mark_label(skip);
    }
    if returns_value {
        let z = b.ir.const_(0);
        b.ret(Some(z));
    } else {
        b.ret(None);
    }
    let name = function_name(inst, kind, true, targets)?;
    Ok(Some(finish_function(b, &name, returns_value, in_args)?))
}
