//! Tracing a load or store address to its storage buffer constant buffer slot and offset, and the local and shared memory offsets.

use super::*;

pub(super) fn get_base_address_cb_with_offset(
    ir: &Ir,
    base: Opd,
    offset: Opd,
    const_offset: i32,
) -> Search {
    if ir.ty(base) == OperandType::ConstantBuffer {
        let (slot, off) = (ir.cbuf_slot(base), ir.cbuf_offset(base));
        if off & 1 == 0 {
            return Search {
                sb_cb_slot: slot,
                sb_cb_offset: off,
                offset: Some(offset),
                const_offset,
            };
        }
    }
    Search::not_found()
}

pub(super) fn find_from_memory(
    ir: &Ir,
    ctx: &Gts,
    op: Option<NodeId>,
    const_offset: i32,
    needs_offset: bool,
) -> R<Search> {
    if let Some(op) = op
        && let Some((ty, bo, co)) = try_get_memory_offsets(ir, op)?
        && let Some(r) = ctx.try_get_memory_target_cb(ty, bo, co)
        && (r.offset.is_some() || !needs_offset)
    {
        if const_offset != 0 {
            return Ok(Search {
                const_offset: r.const_offset + const_offset,
                ..r
            });
        }
        return Ok(r);
    }
    Ok(Search::not_found())
}

pub(super) fn find_unique_base_address_cb(
    ir: &mut Ir,
    blocks: &[Block],
    bi: usize,
    ctx: &Gts,
    global_address: Opd,
    needs_offset: bool,
) -> R<Search> {
    let global_address = utils::find_last_operation(ir, blocks, global_address, bi, true)?;
    if ir.ty(global_address) == OperandType::ConstantBuffer {
        let z = ir.const_(0);
        return Ok(get_base_address_cb_with_offset(ir, global_address, z, 0));
    }
    let mut operation = ir.asg_operation(global_address);
    let Some(op) = operation.filter(|o| ir.op(*o).inst == Inst::Add) else {
        return find_from_memory(ir, ctx, operation, 0, needs_offset);
    };
    let (mut src1, mut src2) = (s(ir, op, 0)?, s(ir, op, 1)?);
    let mut const_offset = 0;
    let (t1, t2) = (ir.ty(src1), ir.ty(src2));
    if (t1 == OperandType::LocalVariable && t2 == OperandType::Constant)
        || (t2 == OperandType::LocalVariable && t1 == OperandType::Constant)
    {
        let (base_addr, offset) = if t1 == OperandType::LocalVariable {
            (
                utils::find_last_operation(ir, blocks, src1, bi, true)?,
                src2,
            )
        } else {
            (
                utils::find_last_operation(ir, blocks, src2, bi, true)?,
                src1,
            )
        };
        let r = get_base_address_cb_with_offset(ir, base_addr, offset, 0);
        if r.found() {
            return Ok(r);
        }
        const_offset = ir.value(offset);
        operation = ir.asg_operation(base_addr);
        match operation.filter(|o| ir.op(*o).inst == Inst::Add) {
            None => return find_from_memory(ir, ctx, operation, const_offset, needs_offset),
            Some(o) => {
                src1 = s(ir, o, 0)?;
                src2 = s(ir, o, 1)?;
            }
        }
    }
    let (c1, c2) = (
        ir.ty(src1) == OperandType::ConstantBuffer,
        ir.ty(src2) == OperandType::ConstantBuffer,
    );
    if c1
        && c2
        && ir.cbuf_slot(src1) == DRIVER_RESERVED_CB
        && ir.cbuf_slot(src2) != DRIVER_RESERVED_CB
    {
        return Ok(get_base_address_cb_with_offset(
            ir,
            src1,
            src2,
            const_offset,
        ));
    }
    if !c1 || (c1 && c2) || (c2 && ir.cbuf_slot(src2) == DRIVER_RESERVED_CB) {
        return Ok(get_base_address_cb_with_offset(
            ir,
            src2,
            src1,
            const_offset,
        ));
    }
    Ok(get_base_address_cb_with_offset(
        ir,
        src1,
        src2,
        const_offset,
    ))
}

pub(super) fn try_get_memory_offsets(ir: &Ir, op: NodeId) -> R<Option<(LsType, Option<Opd>, i32)>> {
    let o = ir.op(op);
    if o.inst == Inst::Load || o.inst == Inst::Store {
        if o.storage_kind == StorageKind::SharedMemory {
            return Ok(shared_offsets(ir, op)?.map(|(b, c)| (LsType::Shared, Some(b), c)));
        } else if o.storage_kind == StorageKind::LocalMemory {
            let off = s(ir, op, 1)?;
            if ir.ty(off) == OperandType::Constant {
                return Ok(Some((LsType::Local, None, ir.value(off))));
            }
            return Ok(None);
        }
    }
    Ok(None)
}

pub(super) fn shared_offsets(ir: &Ir, op: NodeId) -> R<Option<(Opd, i32)>> {
    let mut base: Option<Opd> = None;
    let mut const_offset = 0;
    let src1 = s(ir, op, 1)?;
    if let Some(sr) = ir.asg_operation(src1)
        && ir.op(sr).inst == Inst::ShiftRightU32
    {
        let c = s(ir, sr, 1)?;
        if ir.ty(c) == OperandType::Constant && ir.value(c) == 2 {
            base = Some(s(ir, sr, 0)?);
        }
    }
    if let Some(b) = base
        && let Some(add) = ir.asg_operation(b)
        && ir.op(add).inst == Inst::Add
    {
        let (a, c) = (s(ir, add, 0)?, s(ir, add, 1)?);
        if ir.ty(a) == OperandType::Constant && ir.ty(c) == OperandType::LocalVariable {
            const_offset = ir.value(a);
            base = Some(c);
        } else if ir.ty(a) == OperandType::LocalVariable && ir.ty(c) == OperandType::Constant {
            base = Some(a);
            const_offset = ir.value(c);
        }
    }
    Ok(match base {
        Some(b) if ir.ty(b) == OperandType::LocalVariable => Some((b, const_offset)),
        _ => None,
    })
}
