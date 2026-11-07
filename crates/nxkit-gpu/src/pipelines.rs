//! Pipelines keyed by their translated state (R1, R17).
//!
//! The key is a hash of everything that goes into the Metal pipeline descriptor: the two shader keys, the colour
//! targets with their blend state and write mask, the vertex layout, the topology class, the sample count and the
//! alpha-to-coverage flag. Two draws with the same translated state share one pipeline, however different their NVN
//! state was. The depth and stencil state is a separate object that Metal binds per draw and not per pipeline.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Mutex, PoisonError};

use nxkit_metal::{
    ArchiveConfig, ComputePipeline, Context, DepthStencilDesc, DepthStencilState, Format, PipelineCache,
    RenderPipeline, RenderPipelineDesc,
};

use crate::detail::DrawDetail;
use crate::programs::StageProgram;
use crate::stats::Skip;
use crate::translate;

/// The formats of the colour attachments of a pass by slot, and its sample count.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Targets {
    /// The format of each colour slot. A slot without attachment is `None`.
    pub(crate) colors: Vec<Option<Format>>,
    /// The sample count of the attachments.
    pub(crate) samples: u32,
}

/// The pipeline cache and the depth and stencil states.
pub(crate) struct Pipelines {
    ctx: Context,
    cache: PipelineCache,
    depth: Mutex<HashMap<DepthStencilDesc, DepthStencilState>>,
}

impl Pipelines {
    pub(crate) fn new(ctx: &Context, archive: &ArchiveConfig) -> Result<Self, nxkit_metal::MetalError> {
        Ok(Self { ctx: ctx.clone(), cache: PipelineCache::new(ctx, archive)?, depth: Mutex::new(HashMap::new()) })
    }

    /// The cache that the deswizzle kernels and compute pipelines share.
    pub(crate) fn cache(&self) -> &PipelineCache {
        &self.cache
    }

    /// The number of pipelines made and found so far.
    pub(crate) fn counts(&self) -> (u64, u64) {
        (self.cache.builds(), self.cache.hits())
    }

    /// The pipeline of a draw. `fs` is `None` for a draw without a fragment shader. `icb` asks for a pipeline that an
    /// indirect command buffer may use.
    pub(crate) fn render(
        &self,
        d: &DrawDetail,
        vs: &StageProgram,
        fs: Option<&StageProgram>,
        targets: &Targets,
        icb: bool,
    ) -> Result<(RenderPipeline, u64), Skip> {
        let class = translate::primitive(d.topology).ok_or(Skip::Primitive)?.1;
        let mut desc = RenderPipelineDesc::new(
            (&vs.lib, vs.entry.as_str()),
            fs.map(|f| (&f.lib, f.entry.as_str())),
            &[],
        );
        for (i, f) in targets.colors.iter().enumerate() {
            let format = f.ok_or(Skip::Target)?;
            let written = fs.is_some_and(|f| f.colors.as_ref().is_none_or(|c| c.contains(&(i as u8))));
            let blend = d.blend.get(i).copied().unwrap_or_default();
            let mask = d.write_masks.get(i).copied().unwrap_or(0xF);
            desc.colors.push(translate::color_target(format, &blend, mask, written));
        }
        desc.vertex_layout = translate::vertex_descriptor(d).map_err(|_| Skip::Format)?;
        desc.topology = class;
        desc.sample_count = targets.samples;
        desc.alpha_to_coverage = d.raster.alpha_to_coverage;
        desc.support_icb = icb;
        let mut h = DefaultHasher::new();
        (&d.program.vertex, d.program.fragment.as_deref(), targets, icb).hash(&mut h);
        desc.colors.hash(&mut h);
        desc.vertex_layout.hash(&mut h);
        class.hash(&mut h);
        d.raster.alpha_to_coverage.hash(&mut h);
        let key = h.finish();
        let p = self.cache.render(key, &desc).map_err(|_| Skip::Pipeline)?;
        Ok((p, key))
    }

    /// The compute pipeline of a program.
    pub(crate) fn compute(&self, key: &str, p: &StageProgram) -> Result<ComputePipeline, Skip> {
        let mut h = DefaultHasher::new();
        ("compute", key).hash(&mut h);
        self.cache.compute(h.finish(), &p.lib, &p.entry).map_err(|_| Skip::Pipeline)
    }

    /// The depth and stencil state object of a description, made once.
    pub(crate) fn depth_stencil(&self, d: &DepthStencilDesc) -> Result<DepthStencilState, Skip> {
        let mut map = self.depth.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(s) = map.get(d) {
            return Ok(s.clone());
        }
        let s = DepthStencilState::new(&self.ctx, d).map_err(|_| Skip::Pipeline)?;
        map.insert(*d, s.clone());
        Ok(s)
    }
}
