//! Warp-cooperative instructions of `InstEmitAmpereTensor.cs` (LDSM, MOVM, MATCH, REDUX) and the packed-half compares.

use super::half::half_second;
use super::operand::*;
use crate::emit::alu::helper::{get_fp_comparison, get_pred_logical_op};
use crate::emit::alu::mov::emit_load_subgroup_lane_id;
use crate::ir::Opd;
use crate::ir::emitter::FP32;
use crate::ir::inst::{Inst as I, StorageKind};
use crate::ir::ops::TRUE;
use crate::maxwell::ampere::AmpOp;
use crate::maxwell::enums::*;

fn c(ctx: &mut Ctx, v: i32) -> Opd {
    ctx.ir.const_(v)
}

/// `AGpr`.
fn agpr(ctx: &mut Ctx, r: i32) -> Opd {
    if r >= RZ { c(ctx, 0) } else { gpr(ctx, r) }
}

pub(super) fn redux(ctx: &mut Ctx) -> R {
    let x = x(ctx);
    let op = x.bits(78, 81);
    let signed = x.bit(73);
    if op > 5 {
        return Err(fail(ctx, &format!("REDUX operation {op}")));
    }
    let src = agpr(ctx, x.ra());
    let t = c(ctx, TRUE);
    let active = ctx.ballot(t, 0);
    let mut acc: Option<Opd> = None;
    for lane in 0..32 {
        let l = c(ctx, lane);
        let v = ctx.shuffle(src, l);
        let r = match acc {
            None => v,
            Some(a) => match op {
                0 => ctx.bitwise_and(a, v),
                1 => ctx.bitwise_or(a, v),
                2 => ctx.bitwise_exclusive_or(a, v),
                3 => ctx.i_add(a, v),
                4 => {
                    if signed {
                        ctx.i_minimum_s32(a, v)
                    } else {
                        ctx.i_minimum_u32(a, v)
                    }
                }
                _ => {
                    if signed {
                        ctx.i_maximum_s32(a, v)
                    } else {
                        ctx.i_maximum_u32(a, v)
                    }
                }
            },
        };
        let k = c(ctx, lane);
        let sh = ctx.shift_right_u32(active, k);
        let one = c(ctx, 1);
        let m = ctx.bitwise_and(sh, one);
        let z = c(ctx, 0);
        let on = ctx.i_compare_not_equal(m, z);
        acc = Some(match acc {
            None => {
                let init = match (op, signed) {
                    (0, _) => -1,
                    (4, true) => i32::MAX,
                    (4, false) => -1,
                    (5, true) => i32::MIN,
                    _ => 0,
                };
                let ic = c(ctx, init);
                ctx.conditional_select(on, v, ic)
            }
            Some(a) => ctx.conditional_select(on, r, a),
        });
    }
    if x.rd() != URZ {
        let d = ctx.ir.register(x.rd(), crate::ir::ops::regtype::UNIFORM);
        let a = acc.ok_or("REDUX: no lanes")?;
        ctx.copy_to(d, a);
    }
    Ok(())
}

pub(super) fn match_(ctx: &mut Ctx) -> R {
    let x = x(ctx);
    let (u64_, any) = (x.bit(73), x.bit(79));
    let lo = agpr(ctx, x.ra());
    let hi = if u64_ {
        Some(agpr(ctx, x.ra() + 1))
    } else {
        None
    };
    let t = c(ctx, TRUE);
    let active = ctx.ballot(t, 0);
    let mut same = c(ctx, 0);
    for lane in 0..32 {
        let l = c(ctx, lane);
        let sh = ctx.shuffle(lo, l);
        let mut eq = ctx.i_compare_equal(sh, lo);
        if let Some(h) = hi {
            let l2 = c(ctx, lane);
            let sh2 = ctx.shuffle(h, l2);
            let e2 = ctx.i_compare_equal(sh2, h);
            eq = ctx.bitwise_and(eq, e2);
        }
        let bit = c(ctx, 1i32 << lane);
        let z = c(ctx, 0);
        let sel = ctx.conditional_select(eq, bit, z);
        same = ctx.bitwise_or(same, sel);
    }
    same = ctx.bitwise_and(same, active);
    let all = ctx.i_compare_equal(same, active);
    let res = if any {
        same
    } else {
        let z = c(ctx, 0);
        ctx.conditional_select(all, active, z)
    };
    if x.rd() < RZ {
        let d = gpr(ctx, x.rd());
        ctx.copy_to(d, res);
    }
    if !any {
        set_pred(ctx, x.bits(81, 84), all, false);
    }
    Ok(())
}

/// `.FTZ` (bit 80) of HSETP2 and HSET2: a subnormal half compares as zero. The lanes are unpacked to 32-bit floats, where
/// a half's subnormals are normal, so the bound is the half's own 2^-14.
fn half_ftz(ctx: &mut Ctx, x: &AmpOp, (a, b): ([Opd; 2], [Opd; 2])) -> ([Opd; 2], [Opd; 2]) {
    if !x.bit(80) {
        return (a, b);
    }
    let mut flush = |v: [Opd; 2]| {
        v.map(|l| {
            let m = ctx.fp_absolute(l);
            let k = ctx.ir.const_f(6.103_515_6e-5);
            let small = ctx.fp_compare_less(m, k);
            let z = ctx.ir.const_f(0.0);
            ctx.conditional_select(small, z, l)
        })
    };
    let a = flush(a);
    let b = flush(b);
    (a, b)
}

pub(super) fn hsetp2(ctx: &mut Ctx) -> R {
    let x = x(ctx);
    let ab = half_second(ctx, &x)?;
    let (a, b) = half_ftz(ctx, &x, ab);
    let f = FComp(x.bits(76, 80));
    let mut lo = get_fp_comparison(ctx, f, a[0], b[0], FP32)?;
    let mut hi = get_fp_comparison(ctx, f, a[1], b[1], FP32)?;
    if x.bit(71) {
        lo = ctx.bitwise_and(lo, hi);
        hi = ctx.bitwise_not(lo);
    }
    let p = pred(ctx, &x, 87, 90, false);
    let bop = BoolOp(x.bits(69, 71));
    let v0 = get_pred_logical_op(ctx, bop, lo, p);
    set_pred(ctx, x.bits(81, 84), v0, false);
    let v1 = get_pred_logical_op(ctx, bop, hi, p);
    set_pred(ctx, x.bits(84, 87), v1, false);
    Ok(())
}

pub(super) fn hset2(ctx: &mut Ctx) -> R {
    let x = x(ctx);
    let ab = half_second(ctx, &x)?;
    let (a, b) = half_ftz(ctx, &x, ab);
    let p = pred(ctx, &x, 87, 90, false);
    let bop = BoolOp(x.bits(69, 71));
    let mut res = c(ctx, 0);
    for i in 0..2 {
        let cmp = get_fp_comparison(ctx, FComp(x.bits(76, 80)), a[i], b[i], FP32)?;
        let cond = get_pred_logical_op(ctx, bop, cmp, p);
        let on = c(ctx, if x.bit(71) { 0x3c00 } else { 0xffff });
        let z = c(ctx, 0);
        let v = ctx.conditional_select(cond, on, z);
        let term = if i == 0 {
            v
        } else {
            let k = c(ctx, 16);
            ctx.shift_left(v, k)
        };
        res = ctx.bitwise_or(res, term);
    }
    set(ctx, &x, res, false);
    Ok(())
}

pub(super) fn hmnmx2(ctx: &mut Ctx) -> R {
    let x = x(ctx);
    if x.bit(78) || x.bit(81) || x.bit(82) || x.bit(85) {
        return Err(fail(ctx, "HMNMX2 .F32 / .NAN / .XORSIGN / .BF16"));
    }
    let (a, b) = half_second(ctx, &x)?;
    let p = pred(ctx, &x, 87, 90, false);
    let mut r = [p, p];
    for i in 0..2 {
        let mn = ctx.fp_minimum(a[i], b[i]);
        let mx = ctx.fp_maximum(a[i], b[i]);
        r[i] = ctx.conditional_select(p, mn, mx);
    }
    let v = ctx.pack_half2x16(r[0], r[1]);
    set(ctx, &x, v, false);
    Ok(())
}

fn lane_id(ctx: &mut Ctx) -> Opd {
    emit_load_subgroup_lane_id(ctx)
}

pub(super) fn movm(ctx: &mut Ctx) -> R {
    let x = x(ctx);
    if x.bits(78, 80) != 0 {
        return Err(fail(ctx, &format!("MOVM mode {}", x.bits(78, 80))));
    }
    let lane = lane_id(ctx);
    let k2 = c(ctx, 2);
    let r = ctx.shift_right_u32(lane, k2);
    let k3 = c(ctx, 3);
    let t = ctx.bitwise_and(lane, k3);
    let src = agpr(ctx, x.ra());
    let mut h = [src, src];
    for (j, hj) in h.iter_mut().enumerate() {
        let k1 = c(ctx, 1);
        let t2 = ctx.shift_left(t, k1);
        let jj = c(ctx, j as i32);
        let col = ctx.i_add(t2, jj);
        let k2 = c(ctx, 2);
        let c4 = ctx.shift_left(col, k2);
        let k1b = c(ctx, 1);
        let r2 = ctx.shift_right_u32(r, k1b);
        let idx = ctx.i_add(c4, r2);
        let v = ctx.shuffle(src, idx);
        let one = c(ctx, 1);
        let rm = ctx.bitwise_and(r, one);
        let k4 = c(ctx, 4);
        let off = ctx.shift_left(rm, k4);
        let w = c(ctx, 16);
        *hj = ctx.bitfield_extract_u32(v, off, w);
    }
    if x.rd() < RZ {
        let k = c(ctx, 16);
        let sh = ctx.shift_left(h[1], k);
        let v = ctx.bitwise_or(h[0], sh);
        let d = gpr(ctx, x.rd());
        ctx.copy_to(d, v);
    }
    Ok(())
}

pub(super) fn ldsm(ctx: &mut Ctx) -> R {
    let x = x(ctx);
    let count = 1 << x.bits(72, 74);
    let mode = x.bits(78, 80);
    if count > 4 || mode > 1 {
        return Err(fail(ctx, &format!("LDSM mode {mode} count {count}")));
    }
    let la = super::mem::local_address(ctx, &x, 32, false);
    let k = c(ctx, x.signed_field(40, 64) as i32);
    let addr = ctx.i_add(la, k);
    let id = ctx.rm.shared_memory_id;
    let compute = ctx.tc.definitions.stage == crate::attr::Stage::Compute;
    let gt = if compute {
        None
    } else {
        let lane = lane_id(ctx);
        let k2 = c(ctx, 2);
        let g = ctx.shift_right_u32(lane, k2);
        let k3 = c(ctx, 3);
        Some((g, ctx.bitwise_and(lane, k3)))
    };
    // The warp often stores the rows with no BAR before the LDSM. The Switch runs a warp's shared memory accesses in
    // order, Metal only through a barrier. A SIMD-group barrier before the reads lets the stores land, and one after keeps
    // the next stores away until every lane has read (dotted streaks in Ocarina of Time's movies).
    ctx.add_op(I::SubgroupBarrier, StorageKind::None, None, &[]);
    // In a compute shader one MatrixLoad a matrix, which the tensor pass lowers to the same per-lane code once it knows
    // whether the HMMAs it feeds read their A where it reads. The barriers bracket that code too.
    let mut m = 0;
    while m < count && x.rd() + m < RZ {
        let r = match gt {
            None => {
                let (idc, mc, kc) = (c(ctx, id), c(ctx, mode), c(ctx, m));
                let d = ctx.ir.local();
                ctx.add_op(
                    I::MatrixLoad,
                    StorageKind::None,
                    Some(d),
                    &[idc, mc, addr, kc],
                );
                d
            }
            Some((g, t)) => ldsm_lane(ctx, g, t, addr, id, m, mode),
        };
        let d = gpr(ctx, x.rd() + m);
        ctx.copy_to(d, r);
        m += 1;
    }
    ctx.add_op(I::SubgroupBarrier, StorageKind::None, None, &[]);
    Ok(())
}

/// Where the per-lane code of an LDSM goes: the emitter or the tensor pass.
pub(crate) trait LaneSink {
    fn konst(&mut self, v: i32) -> Opd;
    fn put(&mut self, inst: I, kind: StorageKind, src: &[Opd]) -> Opd;
}

impl LaneSink for Ctx<'_, '_> {
    fn konst(&mut self, v: i32) -> Opd {
        self.ir.const_(v)
    }
    fn put(&mut self, inst: I, kind: StorageKind, src: &[Opd]) -> Opd {
        let d = self.ir.local();
        self.add_op(inst, kind, Some(d), src);
        d
    }
}

/// `LdsmLane`: lane L's register of matrix `m` (g = L / 4, t = L % 4), from the row addresses (bytes in shared memory
/// `id`) the lanes hold in `addr`.
pub(crate) fn ldsm_lane<S: LaneSink>(
    s: &mut S,
    g: Opd,
    t: Opd,
    addr: Opd,
    id: i32,
    m: i32,
    mode: i32,
) -> Opd {
    let bin = |s: &mut S, inst: I, a: Opd, k: i32| {
        let k = s.konst(k);
        s.put(inst, StorageKind::None, &[a, k])
    };
    let load = |s: &mut S, byte_offset: Opd| {
        let k2 = s.konst(2);
        let w = s.put(I::ShiftRightU32, StorageKind::None, &[byte_offset, k2]);
        let idc = s.konst(id);
        s.put(I::Load, StorageKind::SharedMemory, &[idc, w])
    };
    if mode == 0 {
        let src = bin(s, I::Add, g, 8 * m);
        let row_addr = s.put(I::Shuffle, StorageKind::None, &[addr, src]);
        let t4 = bin(s, I::ShiftLeft, t, 2);
        let byte_offset = s.put(I::Add, StorageKind::None, &[row_addr, t4]);
        return load(s, byte_offset);
    }
    let mut h = [addr; 2];
    for (j, hj) in h.iter_mut().enumerate() {
        let t2 = bin(s, I::ShiftLeft, t, 1);
        let row = bin(s, I::Add, t2, j as i32);
        let src = bin(s, I::Add, row, 8 * m);
        let row_addr = s.put(I::Shuffle, StorageKind::None, &[addr, src]);
        let g2 = bin(s, I::ShiftLeft, g, 1);
        let byte_offset = s.put(I::Add, StorageKind::None, &[row_addr, g2]);
        let word = load(s, byte_offset);
        let bm = bin(s, I::BitwiseAnd, byte_offset, 2);
        let off = bin(s, I::ShiftLeft, bm, 3);
        let w16 = s.konst(16);
        *hj = s.put(I::BitfieldExtractU32, StorageKind::None, &[word, off, w16]);
    }
    let sh = bin(s, I::ShiftLeft, h[1], 16);
    s.put(I::BitwiseOr, StorageKind::None, &[h[0], sh])
}
