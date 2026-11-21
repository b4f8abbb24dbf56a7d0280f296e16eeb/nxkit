//! `InstEmitIntegerLogical.cs`, `Lop3Expression.cs`, `InstEmitShift.cs`, `InstEmitPredicate.cs`.

use super::helper::*;
use crate::emit::emitter::EmitterContext;
use crate::emit::helper::*;
use crate::ir::ops::{FALSE, Opd, TRUE, regtype};
use crate::maxwell::enums::*;
use crate::maxwell::insts::*;
use crate::maxwell::names::Emit;

type Ctx<'a, 'b> = EmitterContext<'a, 'b>;
type R = Result<(), String>;

const PT: i32 = 7;

fn emit_lop_pred_write(ctx: &mut Ctx, result: Opd, pred_op: PredicateOp, pred: i32) {
    if pred != PT {
        let p_res = if pred_op == PredicateOp::F {
            ctx.ir.const_(FALSE)
        } else if pred_op == PredicateOp::T {
            ctx.ir.const_(TRUE)
        } else if pred_op == PredicateOp::Z {
            let z = ctx.ir.const_(0);
            ctx.i_compare_equal(result, z)
        } else {
            let z = ctx.ir.const_(0);
            ctx.i_compare_not_equal(result, z)
        };
        let d = ctx.ir.register(pred, regtype::PREDICATE);
        ctx.copy_to(d, p_res);
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_lop(
    ctx: &mut Ctx,
    logic: LogicOp,
    pred_op: PredicateOp,
    a: Opd,
    b: Opd,
    rd: i32,
    dest_pred: i32,
    inv_a: bool,
    inv_b: bool,
    extended: bool,
    wcc: bool,
) {
    let a = ctx.bitwise_not_if(a, inv_a);
    let b = ctx.bitwise_not_if(b, inv_b);
    let res = match logic {
        LogicOp::And => ctx.bitwise_and(a, b),
        LogicOp::Or => ctx.bitwise_or(a, b),
        LogicOp::Xor => ctx.bitwise_exclusive_or(a, b),
        _ => b,
    };
    emit_lop_pred_write(ctx, res, pred_op, dest_pred);
    let d = get_dest(ctx, rd);
    ctx.copy_to(d, res);
    set_zn_flags(ctx, res, wcc, extended);
}

#[allow(clippy::too_many_arguments)]
fn emit_lop3(
    ctx: &mut Ctx,
    table: i32,
    pred_op: PredicateOp,
    a: Opd,
    b: Opd,
    c: Opd,
    rd: i32,
    dest_pred: i32,
    extended: bool,
    wcc: bool,
) -> R {
    let res = get_from_truth_table(ctx, a, b, c, table)
        .ok_or("Lop3: no expression for the truth table")?;
    emit_lop_pred_write(ctx, res, pred_op, dest_pred);
    let d = get_dest(ctx, rd);
    ctx.copy_to(d, res);
    set_zn_flags(ctx, res, wcc, extended);
    Ok(())
}

fn permute_table(imm: i32, bits: [u32; 8]) -> i32 {
    // bits are (bit7, bit6, ..., bit0)
    let mut result = 0;
    for k in 0..8 {
        result |= ((imm >> k) & 1) << bits[7 - k];
    }
    result
}

fn get_expr(imm: i32, ctx: &mut Ctx, x: Opd, y: Opd, z: Opd) -> Option<Opd> {
    Some(match imm {
        0x00 => ctx.ir.const_(0),
        0xff => ctx.ir.const_(-1),
        0xf0 => x,
        0xc0 => ctx.bitwise_and(x, y),
        0xfc => ctx.bitwise_or(x, y),
        0x3c => ctx.bitwise_exclusive_or(x, y),
        0x80 => {
            let t = ctx.bitwise_and(y, z);
            ctx.bitwise_and(x, t)
        }
        0xfe => {
            let t = ctx.bitwise_or(y, z);
            ctx.bitwise_or(x, t)
        }
        0x60 => {
            let t = ctx.bitwise_exclusive_or(y, z);
            ctx.bitwise_and(x, t)
        }
        0xf6 => {
            let t = ctx.bitwise_exclusive_or(y, z);
            ctx.bitwise_or(x, t)
        }
        0xe0 => {
            let t = ctx.bitwise_or(y, z);
            ctx.bitwise_and(x, t)
        }
        0xf8 => {
            let t = ctx.bitwise_and(y, z);
            ctx.bitwise_or(x, t)
        }
        0x16 => {
            let a = ctx.bitwise_or(x, y);
            let b = ctx.bitwise_and(x, y);
            let c = ctx.bitwise_or(z, b);
            ctx.bitwise_exclusive_or(a, c)
        }
        0xe8 => {
            let a = ctx.bitwise_or(x, y);
            let b = ctx.bitwise_and(x, y);
            let c = ctx.bitwise_or(z, b);
            ctx.bitwise_and(a, c)
        }
        0x7e => {
            let a = ctx.bitwise_exclusive_or(x, y);
            let b = ctx.bitwise_exclusive_or(x, z);
            ctx.bitwise_or(a, b)
        }
        0x1a => {
            let a = ctx.bitwise_exclusive_or(x, z);
            let ny = ctx.bitwise_not(y);
            let b = ctx.bitwise_or(ny, z);
            ctx.bitwise_and(a, b)
        }
        0xca => {
            let a = ctx.bitwise_and(x, y);
            let nx = ctx.bitwise_not(x);
            let b = ctx.bitwise_and(nx, z);
            ctx.bitwise_or(a, b)
        }
        0x78 => {
            let t = ctx.bitwise_and(y, z);
            ctx.bitwise_exclusive_or(x, t)
        }
        0x1e => {
            let t = ctx.bitwise_or(y, z);
            ctx.bitwise_exclusive_or(x, t)
        }
        0x96 => {
            let t = ctx.bitwise_exclusive_or(y, z);
            ctx.bitwise_exclusive_or(x, t)
        }
        _ => return None,
    })
}

/// `Lop3Expression.GetFromTruthTable`.
pub(crate) fn get_from_truth_table(ctx: &mut Ctx, a: Opd, b: Opd, c: Opd, imm: i32) -> Option<Opd> {
    for i in 0..0x40 {
        let mut cur = imm;
        let (mut x, mut y, mut z) = (a, b, c);
        if i & 0x01 != 0 {
            std::mem::swap(&mut x, &mut y);
            cur = permute_table(cur, [7, 6, 3, 2, 5, 4, 1, 0]);
        }
        if i & 0x02 != 0 {
            std::mem::swap(&mut x, &mut z);
            cur = permute_table(cur, [7, 3, 5, 1, 6, 2, 4, 0]);
        }
        if i & 0x04 != 0 {
            std::mem::swap(&mut y, &mut z);
            cur = permute_table(cur, [7, 5, 6, 4, 3, 1, 2, 0]);
        }
        if i & 0x08 != 0 {
            x = ctx.bitwise_not(x);
            cur = permute_table(cur, [3, 2, 1, 0, 7, 6, 5, 4]);
        }
        if i & 0x10 != 0 {
            y = ctx.bitwise_not(y);
            cur = permute_table(cur, [5, 4, 7, 6, 1, 0, 3, 2]);
        }
        if i & 0x20 != 0 {
            z = ctx.bitwise_not(z);
            cur = permute_table(cur, [6, 7, 4, 5, 2, 3, 0, 1]);
        }
        if let Some(r) = get_expr(cur, ctx, x, y, z) {
            return Some(r);
        }
        if let Some(n) = get_expr(!cur & 0xff, ctx, x, y, z) {
            return Some(ctx.bitwise_not(n));
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn emit_shf(
    ctx: &mut Ctx,
    max_shift: MaxShift,
    a: Opd,
    b: Opd,
    c: Opd,
    rd: i32,
    mask: bool,
    left: bool,
    wcc: bool,
) {
    let is_long = max_shift == MaxShift::U64 || max_shift == MaxShift::S64;
    let signed_shift = max_shift == MaxShift::S64;
    let max_const = if is_long { 64 } else { 32 };
    let mut b = b;
    if mask {
        let m = ctx.ir.const_(max_const - 1);
        b = ctx.bitwise_and(b, m);
    }
    let mut res;
    if left {
        res = ctx.shift_left(c, b);
        let k32 = ctx.ir.const_(32);
        let sub = ctx.i_subtract(k32, b);
        let sh = ctx.shift_right_u32(a, sub);
        res = ctx.bitwise_or(res, sh);
        if is_long {
            let k32 = ctx.ir.const_(32);
            let sub = ctx.i_subtract(b, k32);
            let lower = ctx.shift_left(a, sub);
            let k32 = ctx.ir.const_(32);
            let gt31 = ctx.i_compare_greater_or_equal_unsigned(b, k32);
            res = ctx.conditional_select(gt31, lower, res);
        }
    } else {
        res = ctx.shift_right_u32(a, b);
        let k32 = ctx.ir.const_(32);
        let sub = ctx.i_subtract(k32, b);
        let sh = ctx.shift_left(c, sub);
        res = ctx.bitwise_or(res, sh);
        if is_long {
            let k32 = ctx.ir.const_(32);
            let sub = ctx.i_subtract(b, k32);
            let upper = if signed_shift {
                ctx.shift_right_s32(c, sub)
            } else {
                ctx.shift_right_u32(c, sub)
            };
            let k32 = ctx.ir.const_(32);
            let gt31 = ctx.i_compare_greater_or_equal_unsigned(b, k32);
            res = ctx.conditional_select(gt31, upper, res);
        }
    }
    if !mask {
        let k = ctx.ir.const_(max_const);
        let lt = ctx.i_compare_less_unsigned(b, k);
        let z = ctx.ir.const_(0);
        res = ctx.conditional_select(lt, res, z);
    }
    let d = get_dest(ctx, rd);
    ctx.copy_to(d, res);
    if wcc {
        set_zn_flags(ctx, res, wcc, false);
    }
}

fn emit_shl(ctx: &mut Ctx, a: Opd, b: Opd, rd: i32, mask: bool) {
    let mut b = b;
    if mask {
        let m = ctx.ir.const_(0x1f);
        b = ctx.bitwise_and(b, m);
    }
    let mut res = ctx.shift_left(a, b);
    if !mask {
        let k = ctx.ir.const_(32);
        let lt = ctx.i_compare_less_unsigned(b, k);
        let z = ctx.ir.const_(0);
        res = ctx.conditional_select(lt, res, z);
    }
    let d = get_dest(ctx, rd);
    ctx.copy_to(d, res);
}

fn emit_shr(ctx: &mut Ctx, a: Opd, b: Opd, rd: i32, mask: bool, brev: bool, signed: bool) {
    let mut a = a;
    let mut b = b;
    if brev {
        a = ctx.bitfield_reverse(a);
    }
    if mask {
        let m = ctx.ir.const_(0x1f);
        b = ctx.bitwise_and(b, m);
    }
    let mut res = if signed {
        ctx.shift_right_s32(a, b)
    } else {
        ctx.shift_right_u32(a, b)
    };
    if !mask {
        let by32 = if signed {
            let k = ctx.ir.const_(31);
            ctx.shift_right_s32(a, k)
        } else {
            ctx.ir.const_(0)
        };
        let k = ctx.ir.const_(32);
        let lt = ctx.i_compare_less_unsigned(b, k);
        res = ctx.conditional_select(lt, res, by32);
    }
    let d = get_dest(ctx, rd);
    ctx.copy_to(d, res);
}

pub(crate) fn emit_p2r(ctx: &mut Ctx, a: Opd, dest: Opd, mask: Opd, byte_sel: ByteSel, ccpr: bool) {
    let count = if ccpr { 4 } else { 7 };
    let shift = byte_sel.0 * 8;
    let m = ctx.ir.const_(0xff);
    let mask = ctx.bitwise_and(mask, m);
    let mut insert = ctx.ir.const_(0);
    for i in 0..count {
        let cond = ctx.ir.register(
            i,
            if ccpr {
                regtype::FLAG
            } else {
                regtype::PREDICATE
            },
        );
        let bitc = ctx.ir.const_(1 << (i + shift));
        let z = ctx.ir.const_(0);
        let bit = ctx.conditional_select(cond, bitc, z);
        insert = ctx.bitwise_or(insert, bit);
    }
    let s = ctx.ir.const_(shift);
    let mask_shifted = ctx.shift_left(mask, s);
    let nm = ctx.bitwise_not(mask_shifted);
    let masked = ctx.bitwise_and(a, nm);
    let ins = ctx.bitwise_and(insert, mask_shifted);
    let res = ctx.bitwise_or(masked, ins);
    ctx.copy_to(dest, res);
}

fn pred_reg_not(ctx: &mut Ctx, p: i32, inv: bool) -> Opd {
    let r = ctx.ir.register(p, regtype::PREDICATE);
    ctx.bitwise_not_if(r, inv)
}

pub(super) fn try_emit(ctx: &mut Ctx, emit: Emit) -> Option<R> {
    let raw = ctx.curr_op.raw;
    macro_rules! lop {
        ($ty:ident, $k:ident, $f:ident) => {{
            let op = $ty(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = src_b!($k, ctx, op);
            emit_lop(
                ctx,
                op.$f(),
                op.predicate_op(),
                a,
                b,
                op.dest(),
                op.dest_pred(),
                op.neg_a(),
                op.neg_b(),
                op.x(),
                op.write_cc(),
            );
        }};
    }
    macro_rules! lop3 {
        ($ty:ident, $k:ident, full) => {{
            let op = $ty(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = src_b!($k, ctx, op);
            let c = get_src_reg(ctx, op.src_c(), false);
            emit_lop3(
                ctx,
                op.imm(),
                op.predicate_op(),
                a,
                b,
                c,
                op.dest(),
                op.dest_pred(),
                op.x(),
                op.write_cc(),
            )
        }};
        ($ty:ident, $k:ident) => {{
            let op = $ty(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = src_b!($k, ctx, op);
            let c = get_src_reg(ctx, op.src_c(), false);
            emit_lop3(
                ctx,
                op.imm(),
                PredicateOp::F,
                a,
                b,
                c,
                op.dest(),
                PT,
                false,
                op.write_cc(),
            )
        }};
    }
    macro_rules! shf {
        ($ty:ident, $left:expr, R) => {{
            let op = $ty(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = get_src_reg(ctx, op.src_b(), false);
            let c = get_src_reg(ctx, op.src_c(), false);
            emit_shf(
                ctx,
                op.max_shift(),
                a,
                b,
                c,
                op.dest(),
                op.m(),
                $left,
                op.write_cc(),
            );
        }};
        ($ty:ident, $left:expr, I) => {{
            let op = $ty(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = ctx.ir.const_(op.imm6());
            let c = get_src_reg(ctx, op.src_c(), false);
            emit_shf(
                ctx,
                op.max_shift(),
                a,
                b,
                c,
                op.dest(),
                op.m(),
                $left,
                op.write_cc(),
            );
        }};
    }
    macro_rules! shl {
        ($ty:ident, $k:ident) => {{
            let op = $ty(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = src_b!($k, ctx, op);
            emit_shl(ctx, a, b, op.dest(), op.m());
        }};
    }
    macro_rules! shr {
        ($ty:ident, $k:ident) => {{
            let op = $ty(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = src_b!($k, ctx, op);
            emit_shr(ctx, a, b, op.dest(), op.m(), op.brev(), op.signed());
        }};
    }
    macro_rules! p2r {
        ($ty:ident, $mask:expr) => {{
            let op = $ty(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let d = get_src_reg(ctx, op.dest(), false);
            let m = $mask(ctx, &op);
            emit_p2r(ctx, a, d, m, op.byte_sel(), op.ccpr());
        }};
    }
    match emit {
        Emit::LopR => lop!(InstLopR, R, lop),
        Emit::LopI => lop!(InstLopI, S, logic_op),
        Emit::LopC => lop!(InstLopC, C, logic_op),
        Emit::Lop32i => {
            let op = InstLop32i(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = get_src_imm(ctx, op.imm32(), false);
            emit_lop(
                ctx,
                op.logic_op(),
                PredicateOp::F,
                a,
                b,
                op.dest(),
                PT,
                op.neg_a(),
                op.neg_b(),
                op.x(),
                op.write_cc(),
            );
        }
        Emit::Lop3R => return Some(lop3!(InstLop3R, R, full)),
        Emit::Lop3I => return Some(lop3!(InstLop3I, S)),
        Emit::Lop3C => return Some(lop3!(InstLop3C, C)),
        Emit::ShfLR => shf!(InstShfLR, true, R),
        Emit::ShfRR => shf!(InstShfRR, false, R),
        Emit::ShfLI => shf!(InstShfLI, true, I),
        Emit::ShfRI => shf!(InstShfRI, false, I),
        Emit::ShlR => shl!(InstShlR, R),
        Emit::ShlI => shl!(InstShlI, S),
        Emit::ShlC => shl!(InstShlC, C),
        Emit::ShrR => shr!(InstShrR, R),
        Emit::ShrI => shr!(InstShrI, S),
        Emit::ShrC => shr!(InstShrC, C),
        Emit::P2rR => p2r!(InstP2rR, |c: &mut Ctx, o: &InstP2rR| get_src_reg(
            c,
            o.src_b(),
            false
        )),
        Emit::P2rI => p2r!(InstP2rI, |c: &mut Ctx, o: &InstP2rI| get_src_imm(
            c,
            o.imm20(),
            false
        )),
        Emit::P2rC => p2r!(InstP2rC, |c: &mut Ctx, o: &InstP2rC| get_src_cbuf(
            c,
            o.cbuf_slot(),
            o.cbuf_offset(),
            false
        )),
        Emit::Pset => {
            let op = InstPset(raw);
            let a = pred_reg_not(ctx, op.src2_pred(), op.src2_pred_inv());
            let b = pred_reg_not(ctx, op.src1_pred(), op.src1_pred_inv());
            let c = pred_reg_not(ctx, op.src_pred(), op.src_pred_inv());
            let mut res = get_pred_logical_op(ctx, op.bool_op_ab(), a, b);
            res = get_pred_logical_op(ctx, op.bool_op_c(), res, c);
            let dest = get_dest(ctx, op.dest());
            if op.b_val() {
                let one = ctx.ir.const_f(1.0);
                let zero = ctx.ir.const_f(0.0);
                let s = ctx.conditional_select(res, one, zero);
                ctx.copy_to(dest, s);
            } else {
                ctx.copy_to(dest, res);
            }
        }
        Emit::Psetp => {
            let op = InstPsetp(raw);
            let a = pred_reg_not(ctx, op.src2_pred(), op.src2_pred_inv());
            let b = pred_reg_not(ctx, op.src1_pred(), op.src1_pred_inv());
            let p0 = get_pred_logical_op(ctx, op.bool_op_ab(), a, b);
            let p1 = ctx.bitwise_not(p0);
            let sp = get_predicate(ctx, op.src_pred(), op.src_pred_inv());
            let p0 = get_pred_logical_op(ctx, op.bool_op_c(), p0, sp);
            let p1 = get_pred_logical_op(ctx, op.bool_op_c(), p1, sp);
            let d0 = ctx.ir.register(op.dest_pred(), regtype::PREDICATE);
            ctx.copy_to(d0, p0);
            let d1 = ctx.ir.register(op.dest_pred_inv(), regtype::PREDICATE);
            ctx.copy_to(d1, p1);
        }
        _ => return None,
    }
    Some(Ok(()))
}
