//! `InstEmitNop.cs`, `InstEmitBarrier.cs`, `InstEmitBitfield.cs` and the not-implemented instructions of `InstEmit.cs`.

use crate::emit::emitter::EmitterContext;
use crate::emit::helper::*;
use crate::ir::ops::Opd;
use crate::maxwell::enums::*;
use crate::maxwell::insts::*;
use crate::maxwell::names::Emit;

type Ctx<'a, 'b> = EmitterContext<'a, 'b>;
type R = Result<(), String>;

fn emit_bfe(ctx: &mut Ctx, a: Opd, b: Opd, rd: i32, brev: bool, signed: bool) {
    let mut a = a;
    if brev {
        a = ctx.bitfield_reverse(a);
    }
    let m = ctx.ir.const_(0xff);
    let position = ctx.bitwise_and(b, m);
    let c8 = ctx.ir.const_(8);
    let c8b = ctx.ir.const_(8);
    let size = ctx.bitfield_extract_u32(b, c8, c8b);
    let res = if signed {
        ctx.bitfield_extract_s32(a, position, size)
    } else {
        ctx.bitfield_extract_u32(a, position, size)
    };
    let d = get_dest(ctx, rd);
    ctx.copy_to(d, res);
}

fn emit_bfi(ctx: &mut Ctx, a: Opd, b: Opd, c: Opd, rd: i32) {
    let m = ctx.ir.const_(0xff);
    let position = ctx.bitwise_and(b, m);
    let c8 = ctx.ir.const_(8);
    let c8b = ctx.ir.const_(8);
    let size = ctx.bitfield_extract_u32(b, c8, c8b);
    let res = ctx.bitfield_insert(c, a, position, size);
    let d = get_dest(ctx, rd);
    ctx.copy_to(d, res);
}

fn emit_flo(ctx: &mut Ctx, src: Opd, rd: i32, invert: bool, sh: bool, signed: bool) {
    let b = ctx.bitwise_not_if(src, invert);
    let res = if sh {
        let r = ctx.bitfield_reverse(b);
        ctx.find_lsb(r)
    } else if signed {
        ctx.find_msbs32(b)
    } else {
        ctx.find_msbu32(b)
    };
    let d = get_dest(ctx, rd);
    ctx.copy_to(d, res);
}

fn emit_popc(ctx: &mut Ctx, src: Opd, rd: i32, invert: bool) {
    let b = ctx.bitwise_not_if(src, invert);
    let res = ctx.bit_count(b);
    let d = get_dest(ctx, rd);
    ctx.copy_to(d, res);
}

pub(super) fn try_emit(ctx: &mut Ctx, emit: Emit) -> Option<R> {
    let raw = ctx.curr_op.raw;
    macro_rules! bfe {
        ($ty:ident, $k:ident) => {{
            let op = $ty(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = src_b!($k, ctx, op);
            emit_bfe(ctx, a, b, op.dest(), op.brev(), op.signed());
        }};
    }
    macro_rules! bfi {
        ($ty:ident, $k:ident) => {{
            let op = $ty(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = src_b!($k, ctx, op);
            let c = get_src_reg(ctx, op.src_c(), false);
            emit_bfi(ctx, a, b, c, op.dest());
        }};
    }
    macro_rules! flo {
        ($ty:ident, $k:ident) => {{
            let op = $ty(raw);
            let s = src_b!($k, ctx, op);
            emit_flo(ctx, s, op.dest(), op.neg_b(), op.sh(), op.signed());
        }};
    }
    macro_rules! popc {
        ($ty:ident, $k:ident) => {{
            let op = $ty(raw);
            let s = src_b!($k, ctx, op);
            emit_popc(ctx, s, op.dest(), op.neg_b());
        }};
    }
    match emit {
        Emit::Nop | Emit::Depbar => {}
        Emit::Bar => {
            let op = InstBar(raw);
            if op.bar_op() == BarOp::Sync {
                ctx.barrier();
            } else {
                ctx.acc
                    .log(&format!("Invalid barrier mode: {}.", op.bar_op().0));
            }
        }
        Emit::Membar => {
            let op = InstMembar(raw);
            if op.membar() == Membar::Cta {
                ctx.group_memory_barrier();
            } else {
                ctx.memory_barrier();
            }
        }
        Emit::BfeR => bfe!(InstBfeR, R),
        Emit::BfeI => bfe!(InstBfeI, S),
        Emit::BfeC => bfe!(InstBfeC, C),
        Emit::BfiR => bfi!(InstBfiR, R),
        Emit::BfiI => bfi!(InstBfiI, S),
        Emit::BfiC => bfi!(InstBfiC, C),
        Emit::BfiRc => {
            let op = InstBfiRc(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = get_src_reg(ctx, op.src_c(), false);
            let c = get_src_cbuf(ctx, op.cbuf_slot(), op.cbuf_offset(), false);
            emit_bfi(ctx, a, b, c, op.dest());
        }
        Emit::FloR => flo!(InstFloR, R),
        Emit::FloI => flo!(InstFloI, S),
        Emit::FloC => flo!(InstFloC, C),
        Emit::PopcR => popc!(InstPopcR, R),
        Emit::PopcI => popc!(InstPopcI, S),
        Emit::PopcC => popc!(InstPopcC, C),
        Emit::AtomCas
        | Emit::AtomsCas
        | Emit::B2r
        | Emit::Bpt
        | Emit::Cctl
        | Emit::Cctll
        | Emit::Cctlt
        | Emit::Cs2r
        | Emit::FchkR
        | Emit::FchkI
        | Emit::FchkC
        | Emit::Getcrsptr
        | Emit::Getlmembase
        | Emit::Ide
        | Emit::IdpR
        | Emit::IdpC
        | Emit::ImadspR
        | Emit::ImadspI
        | Emit::ImadspC
        | Emit::ImadspRc
        | Emit::Jcal
        | Emit::Jmp
        | Emit::Jmx
        | Emit::Lepc
        | Emit::Longjmp
        | Emit::Pexit
        | Emit::Plongjmp
        | Emit::Pret
        | Emit::PrmtR
        | Emit::PrmtI
        | Emit::PrmtC
        | Emit::PrmtRc
        | Emit::R2b
        | Emit::Rtt
        | Emit::Setcrsptr
        | Emit::Setlmembase
        | Emit::Vabsdiff
        | Emit::Vabsdiff4
        | Emit::Vadd
        | Emit::Votevtg
        | Emit::Vset
        | Emit::Vshl
        | Emit::Vshr => {
            ctx.acc
                .log(&format!("Shader instruction {emit:?} is not implemented."));
        }
        _ => return None,
    }
    Some(Ok(()))
}
