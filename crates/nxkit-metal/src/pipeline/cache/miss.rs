//! Why the archive lacks a render pipeline: the archive is asked again with the pipeline's state taken apart.

use objc2_metal::{
    MTL4AlphaToCoverageState, MTL4Archive, MTL4BlendState, MTL4IndirectCommandBufferSupportState,
    MTL4RenderPipelineDescriptor, MTLBlendFactor, MTLBlendOperation, MTLColorWriteMask,
    MTLPixelFormat, MTLPrimitiveTopologyClass,
};

use super::{MissWhy, PipelineCache};
use crate::error::Result;
use crate::pipeline::RenderPipelineDesc;
use crate::state::TopologyClass;

impl PipelineCache {
    /// Why the archive lacks `desc`: asks it for the pair with the build's assumed state, then for that pair with one
    /// group of the pipeline's state set at a time. Costs up to nine archive lookups, so call it for a diagnostic.
    pub fn explain_miss(&self, desc: &RenderPipelineDesc<'_>) -> Result<MissWhy> {
        let mut why = MissWhy::default();
        let Some(a) = self.render_archive(desc) else {
            return Ok(why);
        };
        let ask = |group: Option<&str>| -> Result<bool> {
            let d = self.render_descriptor(desc)?;
            assume_defaults(&d);
            if let Some(g) = group {
                set_group(&d, desc, g);
            }
            Ok(a.newRenderPipelineStateWithDescriptor_error(&d).is_ok())
        };
        why.pair_archived = ask(None)?;
        if !why.pair_archived {
            return Ok(why);
        }
        let set = |g: &str| match g {
            "topology" => desc.topology != TopologyClass::Unspecified,
            "samples" => desc.sample_count != 1,
            "alpha-to-coverage" => desc.alpha_to_coverage,
            "vertex-layout" => desc.vertex_layout.build().is_some(),
            "icb" => desc.support_icb,
            _ => desc.colors.iter().any(Option::is_some),
        };
        for g in GROUPS {
            if set(g) && !ask(Some(g))? {
                why.rejected.push(g);
            }
        }
        Ok(why)
    }
}

/// The state groups [`PipelineCache::explain_miss`] asks about.
const GROUPS: [&str; 8] = [
    "topology",
    "samples",
    "alpha-to-coverage",
    "vertex-layout",
    "color-formats",
    "blend",
    "write-mask",
    "icb",
];

/// The state the build's archive assumes: eight unspecialized colour attachments, no vertex layout, no class, one
/// sample, no alpha to coverage, no ICB support (`plan::script_text_with` of `nxb-archive`).
pub(super) fn assume_defaults(d: &MTL4RenderPipelineDescriptor) {
    d.setVertexDescriptor(None);
    d.setInputPrimitiveTopology(MTLPrimitiveTopologyClass::Unspecified);
    // SAFETY: 1 is a supported sample count.
    unsafe { d.setRasterSampleCount(1) };
    d.setAlphaToCoverageState(MTL4AlphaToCoverageState::Disabled);
    d.setSupportIndirectCommandBuffers(MTL4IndirectCommandBufferSupportState::Disabled);
    unspecialize_colors(d);
}

/// Leaves all eight colour attachments unspecialized: format, write mask and blending.
pub(super) fn unspecialize_colors(d: &MTL4RenderPipelineDescriptor) {
    let colors = d.colorAttachments();
    for i in 0..8 {
        // SAFETY: the index is a plain attachment slot below the limit of 8.
        let a = unsafe { colors.objectAtIndexedSubscript(i) };
        a.setPixelFormat(MTLPixelFormat::Unspecialized);
        a.setWriteMask(MTLColorWriteMask::Unspecialized);
        a.setBlendingState(MTL4BlendState::Unspecialized);
        a.setAlphaBlendOperation(MTLBlendOperation::Unspecialized);
        a.setRgbBlendOperation(MTLBlendOperation::Unspecialized);
        a.setDestinationAlphaBlendFactor(MTLBlendFactor::Unspecialized);
        a.setDestinationRGBBlendFactor(MTLBlendFactor::Unspecialized);
        a.setSourceAlphaBlendFactor(MTLBlendFactor::Unspecialized);
        a.setSourceRGBBlendFactor(MTLBlendFactor::Unspecialized);
    }
}

/// Sets one state group of `desc` on a descriptor that [`assume_defaults`] made.
fn set_group(d: &MTL4RenderPipelineDescriptor, desc: &RenderPipelineDesc<'_>, group: &str) {
    let colors = d.colorAttachments();
    match group {
        "topology" => d.setInputPrimitiveTopology(desc.topology.to_mtl()),
        // SAFETY: the count was checked when the descriptor was built.
        "samples" => unsafe { d.setRasterSampleCount(desc.sample_count as usize) },
        "alpha-to-coverage" => d.setAlphaToCoverageState(MTL4AlphaToCoverageState::Enabled),
        "vertex-layout" => d.setVertexDescriptor(desc.vertex_layout.build().as_deref()),
        "icb" => d.setSupportIndirectCommandBuffers(MTL4IndirectCommandBufferSupportState::Enabled),
        _ => {
            for (i, c) in desc.colors.iter().enumerate() {
                let Some(c) = c else { continue };
                // SAFETY: the index is a plain attachment slot. Metal checks the limit when it compiles.
                let a = unsafe { colors.objectAtIndexedSubscript(i) };
                match group {
                    "color-formats" => a.setPixelFormat(c.format.to_mtl()),
                    "write-mask" => a.setWriteMask(c.mask()),
                    _ => {
                        a.setBlendingState(c.blend_state());
                        if let Some(b) = &c.blend {
                            a.setSourceRGBBlendFactor(b.src_rgb.to_mtl());
                            a.setDestinationRGBBlendFactor(b.dst_rgb.to_mtl());
                            a.setRgbBlendOperation(b.op_rgb.to_mtl());
                            a.setSourceAlphaBlendFactor(b.src_alpha.to_mtl());
                            a.setDestinationAlphaBlendFactor(b.dst_alpha.to_mtl());
                            a.setAlphaBlendOperation(b.op_alpha.to_mtl());
                        }
                    }
                }
            }
        }
    }
}
