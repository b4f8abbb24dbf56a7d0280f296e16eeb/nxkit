//! `InstEmitMemory.cs`: LDC, LDG, LDL, LDS, STG, STL, STS, ATOM, ATOMS, RED.

use super::hlp::{REGISTER_ZERO_INDEX, get_dest, get_src_reg};
pub(crate) use super::memory_bits::{extract_small_int, get_bit_offset};
use super::memory_bits::{get_40_bits_address, insert_small_int};
use crate::attr::Stage;
use crate::emit::emitter::EmitterContext;
use crate::ir::inst::StorageKind;
use crate::ir::ops::{Opd, OperandType, regtype};
use crate::maxwell::enums::{AddressMode, AtomOp, AtomSize, AtomsSize, LsSize, LsSize2};
use crate::maxwell::insts::{
    InstAtom, InstAtoms, InstLdc, InstLdg, InstLdl, InstLds, InstRed, InstStg, InstStl, InstSts,
};

fn ls_size_name(s: i32) -> String {
    const N: [&str; 8] = ["U8", "S8", "U16", "S16", "B32", "B64", "B128", "UB128"];
    N.get(s as usize)
        .map_or_else(|| s.to_string(), |n| n.to_string())
}

fn atom_size_name(s: i32) -> String {
    const N: [&str; 6] = ["U32", "S32", "U64", "F32FtzRn", "F16x2FtzRn", "S64"];
    N.get(s as usize)
        .map_or_else(|| s.to_string(), |n| n.to_string())
}

fn atom_op_name(s: i32) -> String {
    const N: [&str; 9] = [
        "Add", "Min", "Max", "Inc", "Dec", "And", "Or", "Xor", "Exch",
    ];
    match s {
        10 => "Safeadd".to_string(),
        _ => N
            .get(s as usize)
            .map_or_else(|| s.to_string(), |n| n.to_string()),
    }
}

fn imm16_to_sint(imm: i32) -> i32 {
    imm as i16 as i32
}

fn imm24_to_sint(imm: i32) -> i32 {
    (imm << 8) >> 8
}

fn stage_log(ctx: &mut EmitterContext<'_, '_>, inst: &str) {
    let st = ctx.tc.definitions.stage;
    ctx.acc.log(&format!(
        "{inst} instruction is not valid on \"{st:?}\" stage."
    ));
}

/// `context.Copy(new Operand(new Register(ra, Gpr)))`: no RZ conversion.
fn raw_gpr(ctx: &mut EmitterContext<'_, '_>, ra: i32) -> Opd {
    ctx.ir
        .operand(OperandType::Register, (regtype::GPR << 24) | ra)
}

pub(super) fn ldc(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstLdc(ctx.curr_op.raw);
    if op.ls_size().0 > LsSize2::B64.0 {
        ctx.acc.log(&format!(
            "Invalid LDC size: {}.",
            ls_size_name(op.ls_size().0)
        ));
        return Ok(());
    }
    let is_small_int = op.ls_size().0 < LsSize2::B32.0;
    let count = if op.ls_size() == LsSize2::B64 { 2 } else { 1 };
    let mut slot = ctx.ir.const_(op.cbuf_slot());
    let mut src_a = get_src_reg(ctx, op.src_a());
    if op.address_mode() == AddressMode::Is || op.address_mode() == AddressMode::Isl {
        let c16 = ctx.ir.const_(16);
        let c16b = ctx.ir.const_(16);
        let hi = ctx.bitfield_extract_u32(src_a, c16, c16b);
        slot = ctx.i_add(slot, hi);
        let m = ctx.ir.const_(0xffff);
        src_a = ctx.bitwise_and(src_a, m);
    }
    let off = ctx.ir.const_(imm16_to_sint(op.cbuf_offset()));
    let addr = ctx.i_add(src_a, off);
    let c2 = ctx.ir.const_(2);
    let word_offset = ctx.shift_right_u32(addr, c2);
    for index in 0..count {
        let dest = op.dest() + index;
        if dest == REGISTER_ZERO_INDEX {
            break;
        }
        let ci = ctx.ir.const_(index);
        let offset = ctx.i_add(word_offset, ci);
        let mut value = emit_load_constant(ctx, slot, offset, None)?;
        if is_small_int {
            let bo = get_bit_offset(ctx, addr);
            value = extract_small_int(ctx, op.ls_size().0, bo, value);
        }
        let d = ctx.ir.register(dest, regtype::GPR);
        ctx.copy_to(d, value);
    }
    Ok(())
}

/// `EmitLoadConstant`: `banks` are the banks a run-time slot can be (Ampere's LDC .IS). The accessor reports no bound
/// constant buffers (`QueryConstantBufferUse` is 0), so without `banks` a run-time slot selects nothing.
pub(crate) fn emit_load_constant(
    ctx: &mut EmitterContext<'_, '_>,
    slot: Opd,
    offset: Opd,
    banks: Option<u32>,
) -> Result<Opd, String> {
    let c2 = ctx.ir.const_(2);
    let vec_index = ctx.shift_right_u32(offset, c2);
    let c3 = ctx.ir.const_(3);
    let elem_index = ctx.bitwise_and(offset, c3);
    if ctx.ir.ty(slot) == OperandType::Constant {
        let s = ctx.ir.value(slot);
        let binding = ctx.rm.get_constant_buffer_binding(ctx.acc, s)?;
        let z = ctx.ir.const_(0);
        Ok(ctx.load_b3(
            StorageKind::ConstantBuffer,
            binding,
            z,
            vec_index,
            elem_index,
        ))
    } else {
        let mut value = ctx.ir.const_(0);
        let mut mask = banks.unwrap_or(0);
        while mask != 0 {
            let cb_index = mask.trailing_zeros() as i32;
            let binding = ctx.rm.get_constant_buffer_binding(ctx.acc, cb_index)?;
            let ci = ctx.ir.const_(cb_index);
            let is_current = ctx.i_compare_equal(slot, ci);
            let z = ctx.ir.const_(0);
            let current = ctx.load_b3(
                StorageKind::ConstantBuffer,
                binding,
                z,
                vec_index,
                elem_index,
            );
            value = ctx.conditional_select(is_current, current, value);
            mask &= !(1u32 << cb_index);
        }
        Ok(value)
    }
}

pub(super) fn ldg(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstLdg(ctx.curr_op.raw);
    emit_ldg(
        ctx,
        op.ls_size(),
        op.src_a(),
        op.dest(),
        imm24_to_sint(op.imm24()),
        op.e(),
    )
}

pub(super) fn ldl(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstLdl(ctx.curr_op.raw);
    let a = get_src_reg(ctx, op.src_a());
    emit_load(
        ctx,
        StorageKind::LocalMemory,
        op.ls_size(),
        a,
        op.dest(),
        imm24_to_sint(op.imm24()),
    )
}

pub(super) fn lds(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    if ctx.tc.definitions.stage != Stage::Compute {
        stage_log(ctx, "Lds");
        return Ok(());
    }
    let op = InstLds(ctx.curr_op.raw);
    let a = get_src_reg(ctx, op.src_a());
    emit_load(
        ctx,
        StorageKind::SharedMemory,
        op.ls_size(),
        a,
        op.dest(),
        imm24_to_sint(op.imm24()),
    )
}

pub(super) fn stg(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstStg(ctx.curr_op.raw);
    emit_stg(
        ctx,
        op.ls_size(),
        op.src_a(),
        op.dest(),
        imm24_to_sint(op.imm24()),
        op.e(),
    )
}

pub(super) fn stl(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstStl(ctx.curr_op.raw);
    let a = get_src_reg(ctx, op.src_a());
    emit_store(
        ctx,
        StorageKind::LocalMemory,
        op.ls_size(),
        a,
        op.dest(),
        imm24_to_sint(op.imm24()),
    )
}

pub(super) fn sts(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    if ctx.tc.definitions.stage != Stage::Compute {
        stage_log(ctx, "Sts");
        return Ok(());
    }
    let op = InstSts(ctx.curr_op.raw);
    let a = get_src_reg(ctx, op.src_a());
    emit_store(
        ctx,
        StorageKind::SharedMemory,
        op.ls_size(),
        a,
        op.dest(),
        imm24_to_sint(op.imm24()),
    )
}

pub(super) fn atom(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstAtom(ctx.curr_op.raw);
    let s_offset = (op.imm20() << 12) >> 12;
    let (lo, hi) = get_40_bits_address(ctx, op.src_a(), op.e(), s_offset);
    let value = get_src_reg(ctx, op.src_b());
    let res = emit_atomic_op(
        ctx,
        StorageKind::GlobalMemory,
        op.op(),
        op.size(),
        lo,
        hi,
        value,
    );
    let d = get_dest(ctx, op.dest());
    ctx.copy_to(d, res);
    Ok(())
}

pub(super) fn atoms(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    if ctx.tc.definitions.stage != Stage::Compute {
        stage_log(ctx, "Atoms");
        return Ok(());
    }
    let op = InstAtoms(ctx.curr_op.raw);
    let a = get_src_reg(ctx, op.src_a());
    let c2 = ctx.ir.const_(2);
    let mut offset = ctx.shift_right_u32(a, c2);
    let s_offset = (op.imm22() << 10) >> 10;
    let so = ctx.ir.const_(s_offset);
    offset = ctx.i_add(offset, so);
    let value = get_src_reg(ctx, op.src_b());
    let size = match op.atoms_size() {
        AtomsSize::S32 => AtomSize::S32,
        AtomsSize::U64 => AtomSize::U64,
        AtomsSize::S64 => AtomSize::S64,
        _ => AtomSize::U32,
    };
    let id = ctx.ir.const_(ctx.rm.shared_memory_id);
    let res = emit_atomic_op(
        ctx,
        StorageKind::SharedMemory,
        op.atom_op(),
        size,
        id,
        offset,
        value,
    );
    let d = get_dest(ctx, op.dest());
    ctx.copy_to(d, res);
    Ok(())
}

pub(super) fn red(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstRed(ctx.curr_op.raw);
    let (lo, hi) = get_40_bits_address(ctx, op.src_a(), op.e(), op.imm20());
    let v = get_dest(ctx, op.src_b());
    emit_atomic_op(
        ctx,
        StorageKind::GlobalMemory,
        AtomOp(op.red_op().0),
        op.red_size(),
        lo,
        hi,
        v,
    );
    Ok(())
}

/// A float atomic add (RED.F32.FTZ.RN and ATOM's float form) as a compare-and-swap loop on the word. Returns the value
/// before the add.
fn emit_atomic_add_f32(
    ctx: &mut EmitterContext<'_, '_>,
    kind: StorageKind,
    e0: Opd,
    e1: Opd,
    value: Opd,
) -> Opd {
    let retry = ctx.ir.label();
    ctx.mark_label(retry);
    let expected = ctx.load_o2(kind, e0, e1);
    let sum = ctx.fp_add(expected, value);
    let found = ctx.atomic_compare_and_swap_o4(kind, e0, e1, expected, sum);
    let changed = ctx.i_compare_not_equal(found, expected);
    ctx.branch_if_true(retry, changed);
    found
}

pub(crate) fn emit_atomic_op(
    ctx: &mut EmitterContext<'_, '_>,
    kind: StorageKind,
    op: AtomOp,
    ty: AtomSize,
    e0: Opd,
    e1: Opd,
    value: Opd,
) -> Opd {
    let mut res = ctx.ir.const_(0);
    let is32 = ty == AtomSize::S32 || ty == AtomSize::U32;
    let bad = |ctx: &mut EmitterContext<'_, '_>| {
        ctx.acc.log(&format!(
            "Invalid reduction type: {}.",
            atom_size_name(ty.0)
        ));
    };
    match op {
        AtomOp::Add if is32 => res = ctx.atomic_add_o3(kind, e0, e1, value),
        AtomOp::Add if ty == AtomSize::F32FtzRn => {
            res = emit_atomic_add_f32(ctx, kind, e0, e1, value);
        }
        AtomOp::Min if ty == AtomSize::S32 => res = ctx.atomic_min_s32_o3(kind, e0, e1, value),
        AtomOp::Min if ty == AtomSize::U32 => res = ctx.atomic_min_u32_o3(kind, e0, e1, value),
        AtomOp::Max if ty == AtomSize::S32 => res = ctx.atomic_max_s32_o3(kind, e0, e1, value),
        AtomOp::Max if ty == AtomSize::U32 => res = ctx.atomic_max_u32_o3(kind, e0, e1, value),
        AtomOp::And if is32 => res = ctx.atomic_and_o3(kind, e0, e1, value),
        AtomOp::Or if is32 => res = ctx.atomic_or_o3(kind, e0, e1, value),
        AtomOp::Xor if is32 => res = ctx.atomic_xor_o3(kind, e0, e1, value),
        AtomOp::Exch if is32 => res = ctx.atomic_swap_o3(kind, e0, e1, value),
        AtomOp::Add
        | AtomOp::Min
        | AtomOp::Max
        | AtomOp::And
        | AtomOp::Or
        | AtomOp::Xor
        | AtomOp::Exch => bad(ctx),
        _ => ctx.acc.log(&format!(
            "Invalid atomic operation: {}.",
            atom_op_name(op.0)
        )),
    }
    res
}

fn vector_count_2(size: LsSize2) -> i32 {
    match size {
        LsSize2::B64 => 2,
        LsSize2::B128 => 4,
        _ => 1,
    }
}

pub(crate) fn emit_load(
    ctx: &mut EmitterContext<'_, '_>,
    kind: StorageKind,
    size: LsSize2,
    src_a: Opd,
    rd: i32,
    offset: i32,
) -> Result<(), String> {
    if size.0 > LsSize2::B128.0 {
        ctx.acc
            .log(&format!("Invalid load size: {}.", ls_size_name(size.0)));
        return Ok(());
    }
    let id = if kind == StorageKind::LocalMemory {
        ctx.rm.local_memory_id
    } else {
        ctx.rm.shared_memory_id
    };
    let is_small_int = size.0 < LsSize2::B32.0;
    let count = vector_count_2(size);
    let base_offset = ctx.copy(src_a);
    for index in 0..count {
        let dest = rd + index;
        if dest == REGISTER_ZERO_INDEX {
            break;
        }
        let co = ctx.ir.const_(offset + index * 4);
        let byte_offset = ctx.i_add(base_offset, co);
        let c2 = ctx.ir.const_(2);
        let word_offset = ctx.shift_right_u32(byte_offset, c2);
        let bit_offset = get_bit_offset(ctx, byte_offset);
        let mut value = ctx.load_b1(kind, id, word_offset);
        if is_small_int {
            value = extract_small_int(ctx, size.0, bit_offset, value);
        }
        let d = ctx.ir.register(dest, regtype::GPR);
        ctx.copy_to(d, value);
    }
    Ok(())
}

pub(crate) fn get_storage_kind(size: LsSize) -> StorageKind {
    match size {
        LsSize::U8 => StorageKind::GlobalMemoryU8,
        LsSize::S8 => StorageKind::GlobalMemoryS8,
        LsSize::U16 => StorageKind::GlobalMemoryU16,
        LsSize::S16 => StorageKind::GlobalMemoryS16,
        _ => StorageKind::GlobalMemory,
    }
}

pub(crate) fn get_vector_count(size: LsSize) -> i32 {
    match size {
        LsSize::B64 => 2,
        LsSize::B128 | LsSize::UB128 => 4,
        _ => 1,
    }
}

fn emit_ldg(
    ctx: &mut EmitterContext<'_, '_>,
    size: LsSize,
    ra: i32,
    rd: i32,
    offset: i32,
    extended: bool,
) -> Result<(), String> {
    let count = get_vector_count(size);
    let kind = get_storage_kind(size);
    let (_, addr_high) = get_40_bits_address(ctx, ra, extended, offset);
    let r = raw_gpr(ctx, ra);
    let src_a = ctx.copy(r);
    for index in 0..count {
        let dest = rd + index;
        if dest == REGISTER_ZERO_INDEX {
            break;
        }
        let co = ctx.ir.const_(offset + index * 4);
        let a = ctx.i_add(src_a, co);
        let value = ctx.load_o2(kind, a, addr_high);
        let d = ctx.ir.register(dest, regtype::GPR);
        ctx.copy_to(d, value);
    }
    Ok(())
}

pub(crate) fn emit_store(
    ctx: &mut EmitterContext<'_, '_>,
    kind: StorageKind,
    size: LsSize2,
    src_a: Opd,
    rd: i32,
    offset: i32,
) -> Result<(), String> {
    if size.0 > LsSize2::B128.0 {
        ctx.acc
            .log(&format!("Invalid store size: {}.", ls_size_name(size.0)));
        return Ok(());
    }
    let id = if kind == StorageKind::LocalMemory {
        ctx.rm.local_memory_id
    } else {
        ctx.rm.shared_memory_id
    };
    let is_small_int = size.0 < LsSize2::B32.0;
    let count = vector_count_2(size);
    let base_offset = ctx.copy(src_a);
    for index in 0..count {
        let is_rz = rd + index >= REGISTER_ZERO_INDEX;
        let mut value = ctx
            .ir
            .register(if is_rz { rd } else { rd + index }, regtype::GPR);
        let co = ctx.ir.const_(offset + index * 4);
        let byte_offset = ctx.i_add(base_offset, co);
        let c2 = ctx.ir.const_(2);
        let word_offset = ctx.shift_right_u32(byte_offset, c2);
        let bit_offset = get_bit_offset(ctx, byte_offset);
        if is_small_int && kind == StorageKind::LocalMemory {
            let word = ctx.load_b1(kind, id, word_offset);
            value = insert_small_int(ctx, size.0, bit_offset, word, value);
        }
        if kind == StorageKind::LocalMemory {
            ctx.store_b2(kind, id, word_offset, value);
        } else if kind == StorageKind::SharedMemory {
            match size {
                LsSize2::U8 | LsSize2::S8 => {
                    ctx.store_b2(StorageKind::SharedMemory8, id, byte_offset, value)
                }
                LsSize2::U16 | LsSize2::S16 => {
                    ctx.store_b2(StorageKind::SharedMemory16, id, byte_offset, value)
                }
                _ => ctx.store_b2(kind, id, word_offset, value),
            }
        }
    }
    Ok(())
}

fn emit_stg(
    ctx: &mut EmitterContext<'_, '_>,
    size: LsSize2,
    ra: i32,
    rd: i32,
    offset: i32,
    extended: bool,
) -> Result<(), String> {
    if size.0 > LsSize2::B128.0 {
        ctx.acc
            .log(&format!("Invalid store size: {}.", ls_size_name(size.0)));
        return Ok(());
    }
    let ls = LsSize(size.0);
    let count = get_vector_count(ls);
    let kind = get_storage_kind(ls);
    let (_, addr_high) = get_40_bits_address(ctx, ra, extended, offset);
    let r = raw_gpr(ctx, ra);
    let src_a = ctx.copy(r);
    for index in 0..count {
        let is_rz = rd + index >= REGISTER_ZERO_INDEX;
        let value = ctx
            .ir
            .register(if is_rz { rd } else { rd + index }, regtype::GPR);
        let co = ctx.ir.const_(offset + index * 4);
        let addr_low_offset = ctx.i_add(src_a, co);
        ctx.store_o3(kind, addr_low_offset, addr_high, value);
    }
    Ok(())
}

/// The instructions `InstEmit.cs` leaves unimplemented: a log line only.
pub(super) fn not_implemented(ctx: &mut EmitterContext<'_, '_>, name: &str) -> Result<(), String> {
    ctx.acc
        .log(&format!("Shader instruction {name} is not implemented."));
    Ok(())
}
