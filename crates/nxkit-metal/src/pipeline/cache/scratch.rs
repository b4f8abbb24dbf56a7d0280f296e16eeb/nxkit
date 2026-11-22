//! Pipelines that are never kept (W37): they compile on a compiler without a serializer and look in no archive.
//!
//! A shader that a person swapped at run time (`nxkit-gpu` `hotswap`) makes such pipelines. They must not reach the
//! runtime archive of the next start, and no archive holds a pipeline for the new code. The cache keeps them in memory
//! under their key like any other, and they do not count as captured.

use std::sync::{Arc, OnceLock};
use std::time::Instant;

use objc2_metal::{
    MTL4Compiler, MTL4ComputePipelineDescriptor, MTL4IndirectCommandBufferSupportState,
};

use super::{ComputePipeline, Entry, Generation, PipelineCache, RenderPipeline, Source};
use crate::error::{MetalError, Result};
use crate::pipeline::{RenderPipelineDesc, ShaderLibrary};

/// The compiler of the scratch pipelines, made at the first one.
pub(super) type Scratch = OnceLock<std::result::Result<Arc<Generation>, String>>;

impl PipelineCache {
    fn scratch_compiler(&self) -> Result<Arc<Generation>> {
        self.scratch
            .get_or_init(|| {
                Generation::new(&self.ctx, false)
                    .map(Arc::new)
                    .map_err(|e| e.to_string())
            })
            .clone()
            .map_err(MetalError::Compile)
    }

    /// Stores a pipeline that is not captured for an archive.
    fn insert_scratch(&self, key: u64, e: Entry) -> Result<()> {
        self.builds
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let fresh = self
            .entries
            .write()
            .map_err(|_| MetalError::Invalid("pipeline cache lock poisoned"))?
            .insert(key, e)
            .is_none();
        if fresh {
            crate::memstats::pipeline_kept();
        }
        Ok(())
    }

    /// The render pipeline for `key`, compiled from the functions on a compiler without a serializer. No archive is
    /// asked and nothing is captured. The cache holds it, so a second call is a hit.
    pub fn render_scratch(
        &self,
        key: u64,
        desc: &RenderPipelineDesc<'_>,
    ) -> Result<(RenderPipeline, Source)> {
        if let Some(Entry::Render(p)) = self.lookup(key)? {
            return Ok((p, Source::Cached));
        }
        let t0 = Instant::now();
        let d = self.render_descriptor(desc)?;
        let raw = self
            .scratch_compiler()?
            .compiler
            .newRenderPipelineStateWithDescriptor_compilerTaskOptions_error(&d, None)
            .map_err(|e| MetalError::Compile(e.localizedDescription().to_string()))?;
        let p = RenderPipeline { raw };
        self.insert_scratch(key, Entry::Render(p.clone()))?;
        Ok((p, Source::Compiled(t0.elapsed())))
    }

    /// The compute pipeline for `key`, compiled like [`PipelineCache::render_scratch`].
    pub fn compute_scratch(
        &self,
        key: u64,
        lib: &ShaderLibrary,
        function: &str,
    ) -> Result<ComputePipeline> {
        if let Some(Entry::Compute(p)) = self.lookup(key)? {
            return Ok(p);
        }
        let d = MTL4ComputePipelineDescriptor::new();
        let f = lib.function(function)?;
        d.setComputeFunctionDescriptor(Some(&f));
        d.setSupportIndirectCommandBuffers(MTL4IndirectCommandBufferSupportState::Disabled);
        let raw = self
            .scratch_compiler()?
            .compiler
            .newComputePipelineStateWithDescriptor_compilerTaskOptions_error(&d, None)
            .map_err(|e| MetalError::Compile(e.localizedDescription().to_string()))?;
        let p = ComputePipeline { raw };
        self.insert_scratch(key, Entry::Compute(p.clone()))?;
        Ok(p)
    }
}
