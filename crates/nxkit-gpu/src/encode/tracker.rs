//! What the encoder has set on the render pass.

use super::*;

/// What the encoder has set. It starts empty, so the first draw sets everything.
pub(super) struct Tracker {
    size: [u32; 2],
    pub(super) pipeline: Option<u64>,
    pub(super) depth_stencil: Option<DepthStencilDesc>,
    cull: Option<CullMode>,
    pub(super) winding: Option<Winding>,
    pub(super) fill: Option<FillMode>,
    pub(super) bias: Option<DepthBias>,
    pub(super) clip: Option<DepthClip>,
    dynamic: Option<DynamicDetail>,
    pub(super) vertex_args: Option<u64>,
    pub(super) fragment_args: Option<u64>,
    pub(super) object_args: Option<u64>,
    pub(super) streams: [Option<u64>; 32],
}

impl Tracker {
    pub(super) fn new(size: [u32; 2]) -> Self {
        Self {
            size,
            pipeline: None,
            depth_stencil: None,
            cull: None,
            winding: None,
            fill: None,
            bias: None,
            clip: None,
            dynamic: None,
            vertex_args: None,
            fragment_args: None,
            object_args: None,
            streams: [None; 32],
        }
    }

    /// The state that an ICB execution leaves undefined.
    pub(super) fn after_icb(&mut self) {
        let (size, dynamic, cull) = (self.size, self.dynamic, self.cull);
        *self = Self::new(size);
        self.dynamic = dynamic;
        self.cull = cull;
    }

    fn scissor(&self, s: Option<[u32; 4]>, scale: f32) -> Scissor {
        let [w, h] = self.size;
        let Some([x, y, sw, sh]) = s else {
            return Scissor {
                x: 0,
                y: 0,
                width: w,
                height: h,
            };
        };
        let up = |v: u32| (v as f32 * scale).round() as u32;
        let (x, y) = (
            up(x).min(w.saturating_sub(1)),
            up(y).min(h.saturating_sub(1)),
        );
        Scissor {
            x,
            y,
            width: up(sw).max(1).min((w - x).max(1)),
            height: up(sh).max(1).min((h - y).max(1)),
        }
    }

    /// Sets the viewport, the scissor rectangle, the stencil reference, the blend colour and the cull mode.
    pub(super) fn dynamic(
        &mut self,
        rp: &mut RenderPass<'_>,
        pc: &PassCtx,
        d: &DynamicDetail,
        cull: CullMode,
    ) -> Result<(), MetalError> {
        let last = self.dynamic;
        if last.is_none_or(|l| l.viewport != d.viewport || l.depth_range != d.depth_range) {
            let [x, y, w, h] = d.viewport.map(|v| f64::from(v * pc.scale));
            let [near, far] = d.depth_range.map(f64::from);
            rp.encode(&RenderCommand::SetViewport(Viewport {
                x,
                y,
                width: w,
                height: h,
                z_near: near,
                z_far: far,
            }))?;
        }
        if last.is_none_or(|l| l.scissor != d.scissor) {
            rp.encode(&RenderCommand::SetScissor(
                self.scissor(d.scissor, pc.scale),
            ))?;
        }
        if last.is_none_or(|l| l.stencil_ref != d.stencil_ref) {
            rp.encode(&RenderCommand::SetStencilRef {
                front: d.stencil_ref[0],
                back: d.stencil_ref[1],
            })?;
        }
        if last.is_none_or(|l| l.blend_color != d.blend_color) {
            rp.encode(&RenderCommand::SetBlendColor(d.blend_color))?;
        }
        if self.cull != Some(cull) {
            rp.encode(&RenderCommand::SetCullMode(cull))?;
            self.cull = Some(cull);
        }
        self.dynamic = Some(*d);
        Ok(())
    }
}
