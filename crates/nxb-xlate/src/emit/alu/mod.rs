//! Emitters of this family: arithmetic, move, comparison, conversion, logic, shift and flow control.

/// `srcB` of an op by operand format: `R` register, `F` 20-bit float immediate, `S` 20-bit signed immediate, `C` constant
/// buffer and `I32` 32-bit immediate.
macro_rules! src_b {
    (R, $c:expr, $o:expr) => {
        $crate::emit::helper::get_src_reg($c, $o.src_b(), false)
    };
    (F, $c:expr, $o:expr) => {
        $crate::emit::helper::get_src_imm(
            $c,
            $crate::emit::helper::imm20_to_float($o.imm20()),
            false,
        )
    };
    (S, $c:expr, $o:expr) => {
        $crate::emit::helper::get_src_imm(
            $c,
            $crate::emit::helper::imm20_to_sint($o.imm20()),
            false,
        )
    };
    (C, $c:expr, $o:expr) => {
        $crate::emit::helper::get_src_cbuf($c, $o.cbuf_slot(), $o.cbuf_offset(), false)
    };
    (I32, $c:expr, $o:expr) => {
        $crate::emit::helper::get_src_imm($c, $o.imm32(), false)
    };
}

pub(crate) mod cond;
pub(crate) mod conv;
mod float;
pub(crate) mod float_cmp;
mod float_half;
mod flow;
pub mod helper;
mod int_arith;
pub(crate) mod int_cmp;
pub(crate) mod int_logic;
mod misc;
pub(crate) mod mov;
pub(crate) mod video;

use crate::emit::emitter::EmitterContext;
use crate::maxwell::names::Emit;

/// `None` when this family does not handle `emit`.
pub fn try_emit(ctx: &mut EmitterContext<'_, '_>, emit: Emit) -> Option<Result<(), String>> {
    if let Some(r) = float::try_emit(ctx, emit) {
        return Some(r);
    }
    if let Some(r) = float_half::try_emit(ctx, emit) {
        return Some(r);
    }
    if let Some(r) = float_cmp::try_emit(ctx, emit) {
        return Some(r);
    }
    if let Some(r) = cond::try_emit(ctx, emit) {
        return Some(r);
    }
    if let Some(r) = flow::try_emit(ctx, emit) {
        return Some(r);
    }
    if let Some(r) = int_arith::try_emit(ctx, emit) {
        return Some(r);
    }
    if let Some(r) = int_cmp::try_emit(ctx, emit) {
        return Some(r);
    }
    if let Some(r) = int_logic::try_emit(ctx, emit) {
        return Some(r);
    }
    if let Some(r) = conv::try_emit(ctx, emit) {
        return Some(r);
    }
    if let Some(r) = misc::try_emit(ctx, emit) {
        return Some(r);
    }
    if let Some(r) = video::try_emit(ctx, emit) {
        return Some(r);
    }
    mov::try_emit(ctx, emit)
}
