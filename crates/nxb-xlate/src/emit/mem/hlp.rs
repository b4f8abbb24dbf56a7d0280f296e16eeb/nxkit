//! Thin wrappers over `emit/helper.rs` (`InstEmitHelper`) for the integer-register forms the memory family uses.

use crate::emit::emitter::EmitterContext;
use crate::emit::helper;
use crate::ir::ops::Opd;

pub(super) fn get_dest(ctx: &mut EmitterContext<'_, '_>, rd: i32) -> Opd {
    helper::get_dest(ctx, rd)
}

pub(super) fn get_src_reg(ctx: &mut EmitterContext<'_, '_>, reg: i32) -> Opd {
    helper::get_src_reg(ctx, reg, false)
}

/// `RegisterConsts.RegisterZeroIndex`.
pub(super) const REGISTER_ZERO_INDEX: i32 = 255;
