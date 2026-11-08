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

static PIPE_SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

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
        let hit = LAST_PIPELINE.with(|l| {
            l.borrow().as_ref().filter(|m| m.matches(self.id, d, targets, icb)).map(|m| (m.pipeline.clone(), m.key))
        });
        if let Some(hit) = hit {
            return Ok(hit);
        }
        let (pipeline, key) = self.render_uncached(d, vs, fs, targets, icb)?;
        LAST_PIPELINE.with(|l| {
            *l.borrow_mut() = Some(Last {
                owner: self.id,
                icb,
                topology: d.topology,
                alpha_to_coverage: d.raster.alpha_to_coverage,
                program: d.program.clone(),
                targets: targets.clone(),
                blend: d.blend,
                write_masks: d.write_masks,
                attributes: d.attributes.clone(),
                streams: d.streams.iter().map(|s| (s.index, s.stride, s.divisor)).collect(),
                pipeline: pipeline.clone(),
                key,
            });
        });
        Ok((pipeline, key))
    }

    fn render_uncached(
        &self,
        d: &DrawDetail,
        vs: &StageProgram,
        fs: Option<&StageProgram>,
        targets: &Targets,
        icb: bool,
    ) -> Result<(RenderPipeline, u64), Skip> {
        let class = translate::primitive(d.topology).ok_or(Skip::Primitive)?.1;
        let flat = crate::knob("flat") && targets.colors.len() >= 4
            && targets.colors[..4].iter().all(Option::is_some)
            && fs.is_some_and(|f| f.colors.as_deref() == Some(&[0, 1, 2, 3]));
        let flat_lib = if flat {
            let mut library = self.flat.lock().unwrap_or_else(PoisonError::into_inner);
            if library.is_none() {
                *library = Some(ShaderLibrary::from_source(&self.ctx, "nxkit-flat-gbuffer", r#"
                    #include <metal_stdlib>
                    using namespace metal;
                    struct Out { float4 c0 [[color(0)]]; float4 c1 [[color(1)]];
                        float4 c2 [[color(2)]]; float4 c3 [[color(3)]]; };
                    fragment Out flat_gbuffer() { return { float4(0.5, 0.5, 1, 1), float4(1), float4(0.5), float4(0) }; }
                "#).map_err(|_| Skip::Pipeline)?);
            }
            library.clone()
        } else { None };
        let hm_paint = crate::knob("hmpaint")
            && matches!(d.program.fragment.as_deref(), Some("c89a615879087250" | "9a08f70c2775ebe7" | "b7193064f1aa1bdb" | "13540660ad78e484"));
        let paint = hm_paint || (crate::knob("paint") || crate::knob("paintdir"))
            && targets.colors.len() == 1
            && targets.colors[0] == Some(Format::Rg11B10Float)
            && fs.is_some();
        let paint_lib = if paint {
            let mut library = self.paint.lock().unwrap_or_else(PoisonError::into_inner);
            if library.is_none() {
                *library = Some(ShaderLibrary::from_source(&self.ctx, "nxkit-paint", r#"
                    #include <metal_stdlib>
                    using namespace metal;
                    struct In { float4 loc0 [[user(locn0)]]; };
                    fragment float4 paint_magenta() { return float4(8.0, 0.0, 8.0, 1.0); }
                    fragment float4 paint_half() { return float4(0.5, 0.5, 0.5, 0.5); }
                    fragment float4 paint_dir(In in [[stage_in]]) { return float4(abs(in.loc0.xyz), 1.0); }
                "#).map_err(|_| Skip::Pipeline)?);
            }
            library.clone()
        } else { None };
        let cube_test = d.program.fragment.as_deref() == Some("6b1f4b90672b2caf")
            && (crate::knob("cubesrc") || crate::knob("cubesrcs"));
        let cube_lib = if cube_test {
            let mut library = self.cubetest.lock().unwrap_or_else(PoisonError::into_inner);
            if library.is_none() {
                *library = Some(ShaderLibrary::from_source(&self.ctx, "nxkit-cubetest", r#"
                    #include <metal_stdlib>
                    using namespace metal;
                    struct In { float4 loc0 [[user(locn0)]]; };
                    struct Args {
                        constant float4* c3 [[id(0)]];
                        texturecube<float> t [[id(1)]];
                        sampler s [[id(2)]];
                        sampler sw [[id(3)]];
                        constant uint* border [[id(4)]];
                    };
                    float3 face(float4 l) { float3 d = l.xyz; return d / max(max(abs(d.x), abs(d.y)), abs(d.z)); }
                    fragment float4 cube_fixed(In in [[stage_in]], constant Args& a [[buffer(0)]]) {
                        constexpr sampler cs(filter::nearest, mip_filter::none, address::clamp_to_edge);
                        return a.t.sample(cs, face(in.loc0)) + float4(0.2, 0.0, 0.0, 0.0);
                    }
                    fragment float4 cube_bound(In in [[stage_in]], constant Args& a [[buffer(0)]]) {
                        return a.t.sample(a.s, face(in.loc0), level(0)) + float4(0.2, 0.0, 0.0, 0.0);
                    }
                "#).map_err(|_| Skip::Pipeline)?);
            }
            library.clone()
        } else { None };
        let mut desc = RenderPipelineDesc::new(
            (&vs.lib, vs.entry.as_str()),
            cube_lib
                .as_ref()
                .map(|lib| (lib, if crate::knob("cubesrcs") { "cube_bound" } else { "cube_fixed" }))
                .or_else(|| paint_lib
                .as_ref()
                .map(|lib| (lib, if hm_paint { "paint_half" } else if crate::knob("paintdir") { "paint_dir" } else { "paint_magenta" }))
                .or_else(|| flat_lib.as_ref().map(|lib| (lib, "flat_gbuffer")))
                .or_else(|| fs.map(|f| (&f.lib, fragment_entry(f, targets))))),
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
        self.cache
            .compute(h.finish(), &p.lib, &p.entry)
            .map_err(|e| {
                if crate::diag("pipe")
                    && PIPE_SHOWN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 300
                {
                    eprintln!("DIAG pipe compute {}: {e}", p.entry);
                }
                Skip::Pipeline
            })
    }

    /// The depth and stencil state object of a description, made once.
    pub(crate) fn depth_stencil(&self, d: &DepthStencilDesc) -> Result<DepthStencilState, Skip> {
        let mut map = self.depth.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(s) = map.get(d) {
            return Ok(s.clone());
        }
        let s = DepthStencilState::new(&self.ctx, d).map_err(|e| {
            if crate::diag("pipe")
                && PIPE_SHOWN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 300
            {
                eprintln!("DIAG pipe depth-stencil: {e}");
            }
            Skip::Pipeline
        })?;
        map.insert(*d, s.clone());
        Ok(s)
    }
}
