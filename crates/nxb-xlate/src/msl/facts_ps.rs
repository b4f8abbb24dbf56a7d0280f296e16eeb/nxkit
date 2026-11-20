//! The pixel facts: the raster facts of a fragment library and the kill flag of a discard inside a helper function.

use super::emit::Em;
use super::facts::writes;
use super::io::Plan;
use super::refl::{Binding, Raster};
use crate::ir::Inst;
use crate::structured::StructInfo;
use crate::structured::ast::Kind;
use crate::structured::walk::visit;

/// The flag a helper function sets when it discards, so the callers stop at once like the main function does.
pub const KILL: &str = "dm_kill";

/// The raster facts of a pixel shader.
///
/// `discard` is true when the program discards in any function or writes through a resource, because the host then
/// must not move the depth test before the shader. The shader has no way to write a sample mask: the IR has no such
/// output, so `sample_mask` is the plan's flag, which stays false.
pub fn raster(p: &Plan, discards: bool, bindings: &[Binding]) -> Raster {
    let mut colors = p.colors.clone();
    colors.sort_unstable();
    colors.dedup();
    Raster {
        discard: Some(discards || bindings.iter().any(writes)),
        sample_mask: Some(p.sample_mask),
        primitive_id: p.primitive_id,
        colors: Some(colors),
        ..Raster::default()
    }
}

/// Some helper function (not the main function) discards.
pub fn helper_discards(info: &StructInfo) -> bool {
    let mut found = false;
    for f in info.functions.iter().skip(1) {
        visit(info, f.main_block, &mut |_, k| {
            if let Kind::Operation(o) = k {
                found |= Inst(o.inst.0 & Inst::MASK) == Inst::Discard;
            }
        });
    }
    found
}

impl Em<'_> {
    /// The functions carry the kill flag.
    pub(super) fn kill(&self) -> bool {
        self.shared.params.iter().any(|(_, a)| a == KILL)
    }

    /// After a call: a function that was told to stop returns at once.
    pub(super) fn after_call(&mut self) -> super::emit::R<()> {
        if self.kill() {
            self.line(&format!("if ({KILL}) {{"));
            self.ind += 1;
            self.return_default()?;
            self.ind -= 1;
            self.line("}");
        }
        Ok(())
    }
}
