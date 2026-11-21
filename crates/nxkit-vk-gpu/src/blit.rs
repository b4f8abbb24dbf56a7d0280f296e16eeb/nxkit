//! Shows a swapchain image on a drawable of the display: one full-screen triangle reads the image and writes the
//! drawable. The layer is `Bgra8Unorm`, so an image in an sRGB format gets its encode here. A size that differs from
//! the drawable is read by nearest texel. A Vulkan image has its first row at the top. With
//! [`PresentUpscale::MetalFx`] (R70) an image smaller than the drawable is scaled by MetalFX first
//! ([`PresentUpscaler`]), and the triangle reads the scaled image one to one.

use std::sync::Arc;

use nxkit_metal::{
    ArchiveConfig, ArgumentTable, ColorAttachment, Context, Drawable, Format, FrameRing,
    LoadAction, PRESENT_FORMAT, PipelineCache, PresentUpscale, PresentUpscaler, Primitive,
    RenderCommand, RenderPassDesc, RenderPipeline, RenderPipelineDesc, Result, ShaderLibrary,
    StoreAction, Texture, TimedPresent,
};

const MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;
struct V { float4 pos [[position]]; float2 uv; };
vertex V vk_blit_vs(uint id [[vertex_id]]) {
    float2 p = float2((id << 1) & 2, id & 2);
    V v;
    v.pos = float4(p * 2.0 - 1.0, 0.0, 1.0);
    v.uv = float2(p.x, 1.0 - p.y);
    return v;
}
static float4 fetch(texture2d<float, access::read> t, float2 uv) {
    uint2 size = uint2(t.get_width(), t.get_height());
    return t.read(min(uint2(uv * float2(size)), size - 1));
}
fragment float4 vk_blit_plain(V in [[stage_in]], texture2d<float, access::read> t [[texture(0)]]) {
    return fetch(t, in.uv);
}
fragment float4 vk_blit_srgb(V in [[stage_in]], texture2d<float, access::read> t [[texture(0)]]) {
    float4 c = fetch(t, in.uv);
    float3 lo = c.rgb * 12.92;
    float3 hi = 1.055 * pow(max(c.rgb, 0.0), float3(1.0 / 2.4)) - 0.055;
    return float4(select(hi, lo, c.rgb <= 0.0031308), c.a);
}
"#;

/// The present blit, made at the first tick that shows a frame.
pub(crate) struct Blit {
    ctx: Context,
    ring: FrameRing,
    plain: RenderPipeline,
    srgb: RenderPipeline,
    table: ArgumentTable,
    upscaler: PresentUpscaler,
}

impl Blit {
    /// Makes the pipelines and the frame ring of the blit.
    pub(crate) fn new(
        ctx: &Context,
        timed: Option<&Arc<TimedPresent>>,
        upscale: PresentUpscale,
    ) -> Result<Self> {
        let cache = PipelineCache::new(ctx, &ArchiveConfig::default())?;
        let lib = ShaderLibrary::from_source(ctx, "nxkit_vk_blit", MSL)?;
        let pipe = |key, fs| {
            cache.render(
                key,
                &RenderPipelineDesc::new((&lib, "vk_blit_vs"), Some((&lib, fs)), &[PRESENT_FORMAT]),
            )
        };
        let mut ring = FrameRing::new(ctx)?;
        if let Some(t) = timed {
            ring.set_timed_present(t.clone());
        }
        Ok(Self {
            ring,
            ctx: ctx.clone(),
            plain: pipe(1, "vk_blit_plain")?,
            srgb: pipe(2, "vk_blit_srgb")?,
            table: ArgumentTable::new(ctx, 1, 1)?,
            upscaler: PresentUpscaler::new(ctx, upscale),
        })
    }

    /// Draws `src` into `drawable` and presents it at `at` (seconds on the `CACurrentMediaTime` clock), or as soon as
    /// the GPU is done when `at` is None.
    pub(crate) fn present(
        &mut self,
        src: &Texture,
        drawable: Drawable,
        at: Option<f64>,
    ) -> Result<()> {
        let mut frame = self.ring.begin()?;
        let (d, target) = (src.desc(), drawable.texture().desc());
        let scaled = self.upscaler.encode(
            &self.ctx,
            frame.commands(),
            src,
            [0, 0, d.width, d.height],
            (target.width, target.height),
        );
        let src = scaled.as_ref().unwrap_or(src);
        let pipeline = match src.desc().format {
            Format::Rgba8UnormSrgb | Format::Bgra8UnormSrgb => self.srgb.clone(),
            _ => self.plain.clone(),
        };
        let desc = RenderPassDesc {
            colors: vec![Some(ColorAttachment {
                depth_plane: 0,
                texture: drawable.texture().clone(),
                load: LoadAction::DontCare,
                store: StoreAction::Store,
                clear: [0.0; 4],
                memoryless: false,
            })],
            depth: None,
            stencil: None,
        };
        frame.commands().retain(src.clone());
        frame.commands().retain_drawable(&drawable);
        {
            let mut pass = frame.commands().begin_render_pass(&desc, &self.table)?;
            // The game drew the image in an earlier command buffer of the same queue.
            pass.encode(&RenderCommand::WaitForEarlierPasses)?;
            pass.encode(&RenderCommand::SetPipeline(pipeline))?;
            pass.encode(&RenderCommand::SetTexture {
                index: 0,
                texture: src.clone(),
            })?;
            pass.encode(&RenderCommand::Draw {
                primitive: Primitive::Triangle,
                first_vertex: 0,
                vertex_count: 3,
                instance_count: 1,
                base_instance: 0,
            })?;
            pass.end();
        }
        match at {
            Some(at) => frame.present_at(drawable, at),
            None => frame.present(drawable),
        }
        frame.submit()?;
        Ok(())
    }
}
