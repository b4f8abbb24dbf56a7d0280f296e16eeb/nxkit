//! ALU source operands of `InstEmitAmpere.cs`: a register, uniform register, immediate or constant buffer word.

use crate::ir::Inst;
use crate::ir::Opd;
use crate::ir::emitter::EmitterContext;
use crate::ir::ops::regtype;
use crate::maxwell::ampere::AmpOp;
use crate::maxwell::ampere_table::has_uniform_guard;
use crate::msl::abi::{C0_DISPATCH, C0_DISPATCH_NVN2};

pub type Ctx<'a, 'b> = EmitterContext<'a, 'b>;
pub type R = Result<(), String>;

pub const RZ: i32 = 255;
pub const URZ: i32 = 63;
pub const PT: i32 = 7;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AKind {
    Reg,
    UReg,
    Imm,
    Cbuf,
}

/// `ASrc`.
#[derive(Clone, Copy, Debug)]
pub struct ASrc {
    pub kind: AKind,
    /// register number, immediate, or constant buffer byte offset
    pub value: i32,
    pub bank: i32,
    pub neg: bool,
    pub abs: bool,
}

impl ASrc {
    pub fn new(kind: AKind, value: i32) -> ASrc {
        ASrc {
            kind,
            value,
            bank: 0,
            neg: false,
            abs: false,
        }
    }

    pub fn is_zero_reg(&self) -> bool {
        (self.kind == AKind::Reg && self.value == RZ)
            || (self.kind == AKind::UReg && self.value == URZ)
    }
}

/// `AFail`.
pub fn fail(ctx: &Ctx, what: &str) -> String {
    let op = AmpOp::of(&ctx.curr_op);
    format!(
        "Ampere {:03x} at 0x{:x}: {what} ({:016x}{:016x})",
        op.opcode(),
        ctx.curr_op.address,
        op.hi,
        op.lo
    )
}

pub fn x(ctx: &Ctx) -> AmpOp {
    AmpOp::of(&ctx.curr_op)
}

/// `AUniform`: the current instruction is on the uniform datapath.
pub fn is_uniform(ctx: &Ctx) -> bool {
    has_uniform_guard(x(ctx).opcode())
}

/// `AAluSrcs`: A, B, C of an ALU instruction by its form.
pub fn alu_srcs(
    ctx: &Ctx,
    x: &AmpOp,
    uniform: bool,
    neg: bool,
    abs: bool,
) -> Result<(ASrc, ASrc, ASrc), String> {
    let (rk, rbits) = if uniform {
        (AKind::UReg, 6)
    } else {
        (AKind::Reg, 8)
    };
    let reg = |lo: u32, neg_bit: u32, abs_bit: u32| ASrc {
        kind: rk,
        value: x.bits(lo, lo + rbits),
        bank: 0,
        neg: neg && x.bit(neg_bit),
        abs: abs && x.bit(abs_bit),
    };
    let a = reg(24, 72, 73);
    let c64 = reg(64, 75, 74);
    let slot1 = |kind: AKind| -> Result<ASrc, String> {
        Ok(match kind {
            AKind::Reg => reg(32, 63, 62),
            AKind::Imm => ASrc::new(AKind::Imm, x.imm32() as i32),
            AKind::Cbuf => cbuf_operand(ctx, x, neg && x.bit(63), abs && x.bit(62))?,
            AKind::UReg => ASrc {
                kind: AKind::UReg,
                value: x.bits(32, 38),
                bank: 0,
                neg: neg && x.bit(63),
                abs: abs && x.bit(62),
            },
        })
    };
    let form = x.form();
    if uniform {
        return match form {
            1 => Ok((a, slot1(AKind::Reg)?, c64)),
            2 => Ok((a, c64, slot1(AKind::Imm)?)),
            4 => Ok((a, slot1(AKind::Imm)?, c64)),
            _ => Err(fail(ctx, &format!("uniform ALU form {form}"))),
        };
    }
    match form {
        1 => Ok((a, slot1(AKind::Reg)?, c64)),
        2 => Ok((a, c64, slot1(AKind::Imm)?)),
        3 => Ok((a, c64, slot1(AKind::Cbuf)?)),
        4 => Ok((a, slot1(AKind::Imm)?, c64)),
        5 => Ok((a, slot1(AKind::Cbuf)?, c64)),
        6 => Ok((a, slot1(AKind::UReg)?, c64)),
        7 => Ok((a, c64, slot1(AKind::UReg)?)),
        _ => Err(fail(ctx, &format!("ALU form {form}"))),
    }
}

/// `ASecond`: B in forms 1, 4, 5, 6, C in forms 2, 3, 7.
pub fn second(
    ctx: &Ctx,
    x: &AmpOp,
    uniform: bool,
    neg: bool,
    abs: bool,
) -> Result<(ASrc, ASrc), String> {
    let (a, b, c) = alu_srcs(ctx, x, uniform, neg, abs)?;
    Ok((
        a,
        if matches!(x.form(), 1 | 4 | 5 | 6) {
            b
        } else {
            c
        },
    ))
}

/// `AOne`: the only source of a one-source operation, the B slot.
pub fn one(ctx: &Ctx, x: &AmpOp, uniform: bool, neg: bool, abs: bool) -> Result<ASrc, String> {
    Ok(alu_srcs(ctx, x, uniform, neg, abs)?.1)
}

fn cbuf_operand(ctx: &Ctx, x: &AmpOp, neg: bool, abs: bool) -> Result<ASrc, String> {
    if x.bit(91) {
        return Err(fail(
            ctx,
            "constant buffer operand with its bank in a uniform register",
        ));
    }
    let (bank, offset) = map_c0(ctx, x.bits(54, 59), x.bits(38, 54));
    Ok(ASrc {
        kind: AKind::Cbuf,
        value: offset,
        bank,
        neg,
        abs,
    })
}

/// `AMapC0`: NVN2 compute shaders' c0 layout. The workgroup counts at bytes 4, 8 and 12 map to the dispatch words of
/// the driver block (`C0_DISPATCH`).
pub fn map_c0(ctx: &Ctx, bank: i32, offset: i32) -> (i32, i32) {
    let layout = if bank == 0 {
        ctx.acc.query_ampere_compute_c0_layout()
    } else {
        None
    };
    let Some(l) = layout.filter(|l| l.len() >= 5) else {
        return (bank, offset);
    };
    let (ubos, ssbos, textures, size, push) = (l[0], l[1], l[2], l[3], (l[4] + 0xf) & !0xf);
    let ub = 0x10 + push;
    let sb = ub + 0x10 * ubos;
    let hb = sb + 0x10 * ssbos;
    let sep_textures = hb + 8 * textures;
    let sep_samplers = sep_textures + 8 * l.get(5).copied().unwrap_or(0);
    let ib = sep_samplers + 8 * l.get(6).copied().unwrap_or(0);
    let (d0, d1) = (C0_DISPATCH_NVN2 as i32, C0_DISPATCH_NVN2 as i32 + 12);
    if (d0..d1).contains(&offset) {
        // the workgroup counts: the host writes them into the driver block at `C0_DISPATCH`
        (0, C0_DISPATCH as i32 + (offset - d0))
    } else if (0x10..ub).contains(&offset) {
        (24, offset - 0x10)
    } else if (ub..sb).contains(&offset) {
        (0, 0x210 + (offset - ub))
    } else if (sb..hb).contains(&offset) {
        (0, 0x310 + (offset - sb))
    } else if (hb..sep_textures).contains(&offset) {
        (2, 0x20 + (offset - hb))
    } else if (sep_textures..sep_samplers).contains(&offset) {
        (2, 0x168 + (offset - sep_textures))
    } else if (sep_samplers..ib).contains(&offset) {
        (2, 0x568 + (offset - sep_samplers))
    } else if (ib..size).contains(&offset) {
        (2, 0x120 + (offset - ib))
    } else {
        (bank, offset)
    }
}

/// `AGet`: the operand's 32-bit value, modifiers not applied.
pub fn get(ctx: &mut Ctx, ctx_s: &ASrc) -> Result<Opd, String> {
    let s = ctx_s;
    Ok(match s.kind {
        AKind::Reg => ctx.ir.register(s.value, regtype::GPR),
        AKind::UReg => ctx.ir.register(s.value, regtype::UNIFORM),
        AKind::Imm => ctx.ir.const_(s.value),
        AKind::Cbuf => {
            if s.value & 3 != 0 {
                return Err(fail(ctx, "unaligned constant buffer operand"));
            }
            ctx.ir.cbuf(s.bank, s.value >> 2)
        }
    })
}

/// `AGetHigh`: the upper word of a 64-bit operand pair.
pub fn get_high(ctx: &mut Ctx, s: &ASrc) -> Opd {
    match s.kind {
        AKind::Reg if s.value == RZ => ctx.ir.const_(0),
        AKind::Reg => ctx.ir.register(s.value + 1, regtype::GPR),
        AKind::UReg if s.value == URZ => ctx.ir.const_(0),
        AKind::UReg => ctx.ir.register(s.value + 1, regtype::UNIFORM),
        AKind::Imm => ctx.ir.const_(if s.value < 0 { -1 } else { 0 }),
        AKind::Cbuf => ctx.ir.cbuf(s.bank, (s.value >> 2) + 1),
    }
}

/// `AFloat` (FP32).
pub fn float(ctx: &mut Ctx, s: &ASrc) -> Result<Opd, String> {
    float_t(ctx, s, Inst(Inst::FP32))
}

pub fn float_t(ctx: &mut Ctx, s: &ASrc, t: Inst) -> Result<Opd, String> {
    let v = get(ctx, s)?;
    Ok(ctx.fp_abs_neg_t(v, s.abs, s.neg, t))
}

/// `AInt`.
pub fn int(ctx: &mut Ctx, s: &ASrc) -> Result<Opd, String> {
    if s.abs {
        return Err(fail(ctx, "integer absolute value modifier"));
    }
    let v = get(ctx, s)?;
    Ok(if s.neg { ctx.i_negate(v) } else { v })
}

/// The destination register field: 16..24, or 16..22 on the uniform datapath.
pub fn rd_of(x: &AmpOp, uniform: bool) -> i32 {
    if uniform { x.bits(16, 22) } else { x.rd() }
}

/// `ASet`.
pub fn set(ctx: &mut Ctx, x: &AmpOp, value: Opd, uniform: bool) {
    let rd = rd_of(x, uniform);
    let d = ctx.ir.register(
        rd,
        if uniform {
            regtype::UNIFORM
        } else {
            regtype::GPR
        },
    );
    ctx.copy_to(d, value);
}

pub fn pred_reg(ctx: &mut Ctx, index: i32, uniform: bool) -> Opd {
    ctx.ir.register(
        index,
        if uniform {
            regtype::UNIFORM_PREDICATE
        } else {
            regtype::PREDICATE
        },
    )
}

/// `APred`: a predicate source, 3 bits at `lo`, the not bit at `not_bit`.
pub fn pred(ctx: &mut Ctx, x: &AmpOp, lo: u32, not_bit: u32, uniform: bool) -> Opd {
    let p = pred_reg(ctx, x.bits(lo, lo + 3), uniform);
    if x.bit(not_bit) {
        ctx.bitwise_not(p)
    } else {
        p
    }
}

/// `ASetPred`.
pub fn set_pred(ctx: &mut Ctx, index: i32, value: Opd, uniform: bool) {
    if index != PT {
        let p = pred_reg(ctx, index, uniform);
        ctx.copy_to(p, value);
    }
}

pub fn gpr(ctx: &mut Ctx, index: i32) -> Opd {
    ctx.ir.register(index, regtype::GPR)
}
