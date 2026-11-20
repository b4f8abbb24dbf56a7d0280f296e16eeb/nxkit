//! Instruction emitters (`Instructions/InstEmit*.cs`): `dispatch` maps the table's `Emit` value to the emitter.
//!
//! `alu` (arithmetic, move, comparison, conversion, flow control) and `mem` (attributes, memory, textures, surfaces) are
//! written separately: each has a `try_emit` that returns `None` for an `Emit` it does not handle. `emitter` holds the
//! `EmitterContext` (`Translation/EmitterContext.cs`) that the emitters write the IR through; it reads the state of
//! the translation (`crate::translation`), which is why it is here and not in `crate::ir` (W28).

pub mod alu;
pub mod ampere;
pub mod attrmap;
pub mod emitter;
#[rustfmt::skip]
pub mod emitter_gen_0;
#[rustfmt::skip]
pub mod emitter_gen_1;
pub mod emitter_geom;
pub mod emitter_insts;
pub mod emitter_ret;
pub mod helper;
pub mod mem;

use crate::emit::emitter::EmitterContext;
use crate::maxwell::names::Emit;

/// Run the emitter of the current op.
pub fn dispatch(ctx: &mut EmitterContext<'_, '_>, emit: Emit) -> Result<(), String> {
    if let Some(r) = alu::try_emit(ctx, emit) {
        return r;
    }
    if let Some(r) = mem::try_emit(ctx, emit) {
        return r;
    }
    Err(format!("unported: instruction emitter {emit:?}"))
}
