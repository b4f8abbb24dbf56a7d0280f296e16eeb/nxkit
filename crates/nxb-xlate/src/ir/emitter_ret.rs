//! `EmitterContext.EmitStart` and `PrepareForReturn`, for the paths the job accessor can reach (graphics without transform
//! feedback emulation). The geometry and vertex-as-compute parts are in `emitter_geom.rs`.

use super::emitter::EmitterContext;
use super::inst::{IoVariable, StorageKind};
use crate::attr::{Stage, feature};
use crate::translation::definitions::Definitions;
use crate::translation::host;

/// The graphics state of the job accessor has no alpha test, dithering, viewport transform or depth mode (all defaults).
fn check_default_state(d: &Definitions) -> Result<(), String> {
    let _ = d;
    Ok(())
}

/// The register of the depth output: four registers for each render target with any component enabled, then the
/// sample mask register. Counting the enabled components agrees only when every enabled target
/// has all four.
pub fn depth_register(omap_targets: i32) -> i32 {
    let targets = (0..8)
        .filter(|rt| omap_targets & (0xf << (rt * 4)) != 0)
        .count();
    targets as i32 * 4 + 1
}

impl EmitterContext<'_, '_> {
    /// `EmitStart` (constructor tail).
    pub fn emit_start(&mut self) -> Result<(), String> {
        check_default_state(&self.tc.definitions)?;
        if self.tc.definitions.stage == Stage::Vertex {
            let ps = self.ir.const_f(1.0); // GpuGraphicsState.PointSize of the job accessor
            self.store_io(StorageKind::Output, IoVariable::PointSize, None, ps);
        }
        if self.vertex_as_compute {
            self.emit_start_vertex_as_compute()?;
        }
        Ok(())
    }

    /// `PrepareForVertexReturn()`.
    pub fn prepare_for_vertex_return(&mut self) -> Result<(), String> {
        let d = &self.tc.definitions;
        // The job accessor: transform feedback is not enabled, the viewport transform is on and there is no depth mode.
        if !host::SUPPORTS_TRANSFORM_FEEDBACK && host::TRANSFORM_FEEDBACK_ENABLED {
            return Err("unported: transform feedback emulation".into());
        }
        if d.gs_viewport_transform_disable() || d.gs_depth_mode() {
            return Err("unported: viewport transform disable or depth mode".into());
        }
        Ok(())
    }

    /// `PrepareForReturn()`: false is never returned for the supported paths.
    pub fn prepare_for_return(&mut self) -> Result<bool, String> {
        if self.is_non_main {
            return Ok(true);
        }
        let stage = self.tc.definitions.stage;
        if self.tc.definitions.last_in_vertex_pipeline
            && matches!(stage, Stage::Vertex | Stage::TessellationEvaluation)
        {
            self.prepare_for_vertex_return()?;
        } else if stage == Stage::Geometry {
            self.geometry_return()?;
        } else if stage == Stage::Fragment {
            self.fragment_return()?;
        }
        if self.vertex_as_compute {
            self.vertex_as_compute_return()?;
        }
        Ok(true)
    }

    fn fragment_return(&mut self) -> Result<(), String> {
        let omap = self.tc.definitions.omap_targets;
        // GenerateAlphaToCoverageDitherDiscard and the alpha test are off in the job's graphics state.
        if self.tc.definitions.omap_depth {
            let reg = depth_register(omap);
            let src = self.ir.register(reg, crate::ir::regtype::GPR);
            self.store_io(
                StorageKind::Output,
                IoVariable::FragmentOutputDepth,
                None,
                src,
            );
        }
        let mut reg_base = 0;
        // QueryHostSupportsBgraFormat is true: no B/R swap
        for rt in 0..8 {
            for comp in 0..4 {
                if omap & (1 << (rt * 4 + comp)) == 0 {
                    continue;
                }
                let src = self.ir.register(reg_base + comp, crate::ir::regtype::GPR);
                let c0 = self.ir.const_(rt);
                let c1 = self.ir.const_(comp);
                self.store_io_srcs_pub(
                    StorageKind::Output,
                    IoVariable::FragmentOutputColor,
                    None,
                    &[c0, c1, src],
                );
            }
            if omap & (0xf << (rt * 4)) != 0 {
                reg_base += 4;
            }
        }
        Ok(())
    }

    /// `UsesViewportMask`.
    pub fn uses_viewport_mask(&self) -> bool {
        self.tc.program.used_features & feature::VIEWPORT_MASK != 0
    }
}

#[cfg(test)]
mod tests {
    use super::depth_register;

    #[test]
    fn the_depth_register_follows_four_registers_per_target() {
        assert_eq!(depth_register(0), 1);
        assert_eq!(depth_register(0xf), 5);
        // RT0 is R0..R3 and RT2 is R4..R7, then the sample mask R8 and the depth R9
        assert_eq!(depth_register(0x10f), 9);
        assert_eq!(depth_register(0x100), 5);
        assert_eq!(depth_register(0x3f), 9);
        assert_eq!(depth_register(0x30), 5);
        assert_eq!(depth_register(i32::MIN | 1), 9);
    }
}
