//! `InstEmitMove.cs`.

use crate::attr::Stage;
use crate::emit::helper::*;
use crate::ir::emitter::EmitterContext;
use crate::ir::inst::{IoVariable, StorageKind};
use crate::ir::ops::{Opd, regtype};
use crate::maxwell::enums::*;
use crate::maxwell::insts::*;
use crate::maxwell::names::Emit;
use crate::translation::host;

type Ctx<'a, 'b> = EmitterContext<'a, 'b>;
type R = Result<(), String>;

pub(crate) fn emit_r2p(ctx: &mut Ctx, value: Opd, mask: Opd, byte_sel: ByteSel, ccpr: bool) {
    fn test(ctx: &mut Ctx, value: Opd, bit: i32) -> Opd {
        let m = ctx.ir.const_(1 << bit);
        let a = ctx.bitwise_and(value, m);
        let z = ctx.ir.const_(0);
        ctx.i_compare_not_equal(a, z)
    }
    let count = if ccpr { 4 } else { 7 };
    let ty = if ccpr {
        regtype::FLAG
    } else {
        regtype::PREDICATE
    };
    let shift = byte_sel.0 * 8;
    for bit in 0..count {
        let flag = ctx.ir.register(bit, ty);
        let m = test(ctx, mask, bit);
        let v = test(ctx, value, bit + shift);
        let res = ctx.conditional_select(m, v, flag);
        ctx.copy_to(flag, res);
    }
}

fn emit_sel(ctx: &mut Ctx, a: Opd, b: Opd, pred: Opd, rd: i32) {
    let res = ctx.conditional_select(pred, a, b);
    let d = get_dest(ctx, rd);
    ctx.copy_to(d, res);
}

fn load_in(ctx: &mut Ctx, io: IoVariable, elem: i32) -> Opd {
    let c = ctx.ir.const_(elem);
    ctx.load_io_e(StorageKind::Input, io, None, c)
}

pub(crate) fn emit_load_subgroup_lane_id(ctx: &mut Ctx) -> Opd {
    let l = ctx.load_io(StorageKind::Input, IoVariable::SubgroupLaneId, None);
    if host::SUBGROUP_SIZE <= 32 {
        return l;
    }
    let m = ctx.ir.const_(0x1f);
    ctx.bitwise_and(l, m)
}

fn emit_load_subgroup_mask(ctx: &mut Ctx, io: IoVariable) -> Opd {
    let size = host::SUBGROUP_SIZE;
    if size <= 32 {
        load_in(ctx, io, 0)
    } else if size == 64 {
        let lane = ctx.load_io(StorageKind::Input, IoVariable::SubgroupLaneId, None);
        let low = load_in(ctx, io, 0);
        let high = load_in(ctx, io, 1);
        let m = ctx.ir.const_(32);
        let c = ctx.bitwise_and(lane, m);
        ctx.conditional_select(c, high, low)
    } else {
        let lane = ctx.load_io(StorageKind::Input, IoVariable::SubgroupLaneId, None);
        let five = ctx.ir.const_(5);
        let element = ctx.shift_right_u32(lane, five);
        let mut res = load_in(ctx, io, 0);
        for i in 1..4 {
            let k = ctx.ir.const_(i);
            let eq = ctx.i_compare_equal(element, k);
            let v = load_in(ctx, io, i);
            res = ctx.conditional_select(eq, v, res);
        }
        res
    }
}

pub(crate) fn emit_sreg(ctx: &mut Ctx, sreg: SReg) -> Result<Opd, String> {
    let stage = ctx.tc.definitions.stage;
    Ok(match sreg {
        SReg::LaneId => emit_load_subgroup_lane_id(ctx),
        SReg::InvocationId => ctx.load_io(StorageKind::Input, IoVariable::InvocationId, None),
        SReg::YDirection => ctx.ir.const_f(1.0),
        SReg::ThreadKill => {
            if stage == Stage::Fragment {
                ctx.load_io(StorageKind::Input, IoVariable::ThreadKill, None)
            } else {
                ctx.ir.const_(0)
            }
        }
        SReg::InvocationInfo => {
            if stage != Stage::Compute && stage != Stage::Fragment {
                if stage == Stage::TessellationControl || stage == Stage::TessellationEvaluation {
                    let pv = ctx.load_io(StorageKind::Input, IoVariable::PatchVertices, None);
                    let s = ctx.ir.const_(16);
                    ctx.shift_left(pv, s)
                } else {
                    let count = ctx.tc.definitions.input_vertex_count();
                    ctx.ir.const_(count << 16)
                }
            } else {
                ctx.ir.const_(0)
            }
        }
        SReg::TId => {
            let x = load_in(ctx, IoVariable::ThreadId, 0);
            let y = load_in(ctx, IoVariable::ThreadId, 1);
            let z = load_in(ctx, IoVariable::ThreadId, 2);
            let s16 = ctx.ir.const_(16);
            let y = ctx.shift_left(y, s16);
            let s26 = ctx.ir.const_(26);
            let z = ctx.shift_left(z, s26);
            let yz = ctx.bitwise_or(y, z);
            ctx.bitwise_or(x, yz)
        }
        SReg::TIdX => load_in(ctx, IoVariable::ThreadId, 0),
        SReg::TIdY => load_in(ctx, IoVariable::ThreadId, 1),
        SReg::TIdZ => load_in(ctx, IoVariable::ThreadId, 2),
        SReg::CtaIdX => load_in(ctx, IoVariable::CtaId, 0),
        SReg::CtaIdY => load_in(ctx, IoVariable::CtaId, 1),
        SReg::CtaIdZ => load_in(ctx, IoVariable::CtaId, 2),
        SReg::EqMask => emit_load_subgroup_mask(ctx, IoVariable::SubgroupEqMask),
        SReg::LtMask => emit_load_subgroup_mask(ctx, IoVariable::SubgroupLtMask),
        SReg::LeMask => emit_load_subgroup_mask(ctx, IoVariable::SubgroupLeMask),
        SReg::GtMask => emit_load_subgroup_mask(ctx, IoVariable::SubgroupGtMask),
        SReg::GeMask => emit_load_subgroup_mask(ctx, IoVariable::SubgroupGeMask),
        _ => ctx.ir.const_(0),
    })
}

fn pixld(ctx: &mut Ctx) {
    let op = InstPixld(ctx.curr_op.raw);
    // The engine: PIXLD.MYINDEX reads the index of the sample the fragment runs for
    if op.pix_mode() == PixMode::MyIndex
        && op.src_a() == 255
        && op.imm8() == 0
        && op.dest_pred() == 7
        && ctx.tc.definitions.stage == Stage::Fragment
    {
        let d = get_dest(ctx, op.dest());
        let v = ctx.load_io(StorageKind::Input, IoVariable::SampleId, None);
        ctx.copy_to(d, v);
        return;
    }
    ctx.acc.log(&format!(
        "Shader instruction Pixld ({}) is not implemented.",
        op.pix_mode().0
    ));
}

pub(super) fn try_emit(ctx: &mut Ctx, emit: Emit) -> Option<R> {
    let raw = ctx.curr_op.raw;
    match emit {
        Emit::MovR => {
            let op = InstMovR(raw);
            let d = get_dest(ctx, op.dest());
            let s = get_src_reg(ctx, op.src_a(), false);
            ctx.copy_to(d, s);
        }
        Emit::MovI => {
            let op = InstMovI(raw);
            let d = get_dest(ctx, op.dest());
            let s = get_src_imm(ctx, op.imm20(), false);
            ctx.copy_to(d, s);
        }
        Emit::MovC => {
            let op = InstMovC(raw);
            let d = get_dest(ctx, op.dest());
            let s = get_src_cbuf(ctx, op.cbuf_slot(), op.cbuf_offset(), false);
            ctx.copy_to(d, s);
        }
        Emit::Mov32i => {
            let op = InstMov32i(raw);
            let d = get_dest(ctx, op.dest());
            let s = get_src_imm(ctx, op.imm32(), false);
            ctx.copy_to(d, s);
        }
        Emit::R2pR => {
            let op = InstR2pR(raw);
            let v = get_src_reg(ctx, op.src_a(), false);
            let m = get_src_reg(ctx, op.src_b(), false);
            emit_r2p(ctx, v, m, op.byte_sel(), op.ccpr());
        }
        Emit::R2pI => {
            let op = InstR2pI(raw);
            let v = get_src_reg(ctx, op.src_a(), false);
            let m = get_src_imm(ctx, imm20_to_sint(op.imm20()), false);
            emit_r2p(ctx, v, m, op.byte_sel(), op.ccpr());
        }
        Emit::R2pC => {
            let op = InstR2pC(raw);
            let v = get_src_reg(ctx, op.src_a(), false);
            let m = get_src_cbuf(ctx, op.cbuf_slot(), op.cbuf_offset(), false);
            emit_r2p(ctx, v, m, op.byte_sel(), op.ccpr());
        }
        Emit::Pixld => pixld(ctx),
        Emit::S2r => {
            let op = InstS2r(raw);
            let d = get_dest(ctx, op.dest());
            return Some(emit_sreg(ctx, op.s_reg()).map(|s| ctx.copy_to(d, s)));
        }
        Emit::SelR => {
            let op = InstSelR(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = get_src_reg(ctx, op.src_b(), false);
            let p = get_predicate(ctx, op.src_pred(), op.src_pred_inv());
            emit_sel(ctx, a, b, p, op.dest());
        }
        Emit::SelI => {
            let op = InstSelI(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = get_src_imm(ctx, imm20_to_sint(op.imm20()), false);
            let p = get_predicate(ctx, op.src_pred(), op.src_pred_inv());
            emit_sel(ctx, a, b, p, op.dest());
        }
        Emit::SelC => {
            let op = InstSelC(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = get_src_cbuf(ctx, op.cbuf_slot(), op.cbuf_offset(), false);
            let p = get_predicate(ctx, op.src_pred(), op.src_pred_inv());
            emit_sel(ctx, a, b, p, op.dest());
        }
        _ => return None,
    }
    Some(Ok(()))
}
