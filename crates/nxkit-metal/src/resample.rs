//! Scaling one level of a texture into the same level of a larger or smaller texture of the same format.
//!
//! The frame encoder uses it for a guest upload into a render target that is bigger than the guest texture: the
//! deswizzle fills a staging texture of the guest size and this kernel stretches each level and slice into the
//! target. Colour formats that a shader reads as floats are filtered linearly. Integer formats take the nearest
//! texel, since filtering would mix codes. Depth, stencil and compressed formats are not supported (`supports`).
//! The MSL is compiled at run time.

use crate::compute::{ComputeCommand, ComputePass};
use crate::device::Context;
use crate::error::{MetalError, Result};
use crate::format::SampleType;
use crate::pipeline::{ComputePipeline, PipelineCache, ShaderLibrary};
use crate::table::ArgumentTable;
use crate::texture::{Texture, TextureDesc, TextureKind, ViewDesc};

const KEY: u64 = 0x6e78_6b69_745f_7273; // "nxkit_rs"

/// The MSL source of the kernels: `resample_float`, `resample_uint` and `resample_sint`.
pub const RESAMPLE_MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;

constexpr sampler linear_clamp(coord::normalized, address::clamp_to_edge, filter::linear);

kernel void resample_float(texture2d<float, access::sample> src [[texture(0)]],
                           texture2d<float, access::write> dst [[texture(1)]],
                           uint2 gid [[thread_position_in_grid]]) {
    uint w = dst.get_width();
    uint h = dst.get_height();
    if (gid.x >= w || gid.y >= h) { return; }
    float2 uv = (float2(gid) + 0.5) / float2(w, h);
    dst.write(src.sample(linear_clamp, uv, level(0)), gid);
}

kernel void resample_uint(texture2d<uint, access::read> src [[texture(0)]],
                          texture2d<uint, access::write> dst [[texture(1)]],
                          uint2 gid [[thread_position_in_grid]]) {
    uint w = dst.get_width();
    uint h = dst.get_height();
    if (gid.x >= w || gid.y >= h) { return; }
    uint2 at = min(uint2((float2(gid) + 0.5) * float2(src.get_width(), src.get_height()) / float2(w, h)),
                   uint2(src.get_width() - 1, src.get_height() - 1));
    dst.write(src.read(at), gid);
}

kernel void resample_sint(texture2d<int, access::read> src [[texture(0)]],
                          texture2d<int, access::write> dst [[texture(1)]],
                          uint2 gid [[thread_position_in_grid]]) {
    uint w = dst.get_width();
    uint h = dst.get_height();
    if (gid.x >= w || gid.y >= h) { return; }
    uint2 at = min(uint2((float2(gid) + 0.5) * float2(src.get_width(), src.get_height()) / float2(w, h)),
                   uint2(src.get_width() - 1, src.get_height() - 1));
    dst.write(src.read(at), gid);
}
"#;

/// The scaling kernels and the argument table of their passes.
pub struct Resampler {
    float: ComputePipeline,
    uint: ComputePipeline,
    sint: ComputePipeline,
    table: ArgumentTable,
}

// SAFETY: the kernels are immutable pipeline states and one argument table. The caller encodes through one
// resampler on one thread at a time, as the docs of `table` say.
unsafe impl Send for Resampler {}
// SAFETY: see `Send`. A shared reference only reads the table and the pipelines.
unsafe impl Sync for Resampler {}

impl Resampler {
    /// Compiles the kernels through the pipeline cache.
    pub fn new(ctx: &Context, cache: &PipelineCache) -> Result<Self> {
        let lib = ShaderLibrary::from_source(ctx, "nxkit-resample", RESAMPLE_MSL)?;
        Ok(Self {
            float: cache.compute(KEY, &lib, "resample_float")?,
            uint: cache.compute(KEY + 1, &lib, "resample_uint")?,
            sint: cache.compute(KEY + 2, &lib, "resample_sint")?,
            table: ArgumentTable::new(ctx, 1, 2)?,
        })
    }

    /// The table that a compute pass must be started with to use [`Resampler::encode`].
    pub fn table(&self) -> &ArgumentTable {
        &self.table
    }

    /// True when a texture of this description can be the target of [`Resampler::encode`].
    pub fn supports(d: &TextureDesc) -> bool {
        d.shader_write
            && matches!(d.kind, TextureKind::D2 | TextureKind::D2Array)
            && !d.format.is_depth()
            && !d.format.has_stencil()
            && !d.format.is_compressed()
    }

    /// Encodes the scaling of one level and slice of `src` into the same level and slice of `dst`. Both have the same
    /// format. `src` needs `shader_read`, `dst` needs `shader_write`, and both need `allow_views`. Whatever wrote
    /// `src` must have finished (`ComputeCommand::WaitForEarlierPasses`).
    pub fn encode(
        &self,
        pass: &mut ComputePass<'_>,
        src: &Texture,
        dst: &Texture,
        level: u32,
        slice: u32,
    ) -> Result<()> {
        let (sd, dd) = (src.desc(), dst.desc());
        if sd.format != dd.format || !Self::supports(&dd) || !sd.shader_read {
            return Err(MetalError::Invalid("the textures cannot be resampled"));
        }
        if level >= sd.mip_levels.min(dd.mip_levels)
            || slice >= sd.slice_count().min(dd.slice_count())
        {
            return Err(MetalError::Invalid(
                "the level or slice is outside the textures",
            ));
        }
        let view = |t: &Texture| {
            t.view(&ViewDesc {
                format: sd.format,
                kind: TextureKind::D2,
                first_level: level,
                level_count: 1,
                first_slice: slice,
                slice_count: 1,
            })
        };
        let (src_view, dst_view) = (view(src)?, view(dst)?);
        let pipeline = match sd.format.sample_type() {
            SampleType::Float => &self.float,
            SampleType::Uint => &self.uint,
            SampleType::Sint => &self.sint,
        };
        pass.encode(&ComputeCommand::SetPipeline(pipeline.clone()))?;
        pass.encode(&ComputeCommand::SetTexture {
            index: 0,
            texture: src_view.clone(),
        })?;
        pass.encode(&ComputeCommand::SetTexture {
            index: 1,
            texture: dst_view.clone(),
        })?;
        let [w, h, _] = dd.level_extent(level);
        let width = pipeline.thread_execution_width().max(1) as u32;
        let height = (pipeline.max_threads_per_threadgroup() as u32 / width).clamp(1, 8);
        pass.encode(&ComputeCommand::DispatchThreads {
            grid: [w, h, 1],
            threads: [width, height, 1],
        })?;
        pass.retain(src_view);
        pass.retain(dst_view);
        Ok(())
    }
}
