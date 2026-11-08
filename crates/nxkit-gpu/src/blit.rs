//! Shows a texture of the game on a drawable of the display.
//!
//! One full-screen triangle reads the game texture and writes the drawable. The layer is `Bgra8Unorm`, so a game
//! texture in an sRGB format gets its encode here. A size that differs from the drawable is read by nearest pixel. Only
//! the window's crop is shown when the game set one (a 1080p picture in the top left of a 1440p window texture).

use nxkit_metal::{
    ArchiveConfig, ArgumentTable, ColorAttachment, Context, Drawable, Format, FrameRing, LoadAction, PRESENT_FORMAT,
    PipelineCache, Primitive, RenderCommand, RenderPassDesc, RenderPipeline, RenderPipelineDesc, Result,
    ShaderLibrary, StoreAction, Texture,
};

const MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;
struct V { float4 pos [[position]]; float2 uv; };
vertex V blit_vs(uint id [[vertex_id]]) {
    float2 p = float2((id << 1) & 2, id & 2);
    V v;
    v.pos = float4(p * 2.0 - 1.0, 0.0, 1.0);
    v.uv = float2(p.x, 1.0 - p.y);
    return v;
}
// crop: x, y, width, height of the part of the texture to show; a zero size shows all of it.
static float4 fetch(texture2d<float, access::read> t, float2 uv, constant int4& crop) {
    float2 size = float2(t.get_width(), t.get_height());
    float2 org = float2(0.0);
    if (crop.z > 0 && crop.w > 0) {
        org = float2(crop.xy);
        size = min(float2(crop.zw), float2(t.get_width(), t.get_height()) - org);
    }
    uint2 p = uint2(org + clamp(uv * size, float2(0.0), size - 1.0));
    return t.read(p);
}
fragment float4 blit_plain(V in [[stage_in]], texture2d<float, access::read> t [[texture(0)]], constant int4& crop [[buffer(0)]]) {
    return fetch(t, in.uv, crop);
}
fragment float4 blit_srgb(V in [[stage_in]], texture2d<float, access::read> t [[texture(0)]], constant int4& crop [[buffer(0)]]) {
    float4 c = fetch(t, in.uv, crop);
    float3 lo = c.rgb * 12.92;
    float3 hi = 1.055 * pow(max(c.rgb, 0.0), float3(1.0 / 2.4)) - 0.055;
    return float4(select(hi, lo, c.rgb <= 0.0031308), c.a);
}
"#;

pub(crate) struct Blit {
    ring: FrameRing,
    plain: RenderPipeline,
    srgb: RenderPipeline,
    table: ArgumentTable,
}

impl Blit {
    pub(crate) fn new(ctx: &Context) -> Result<Self> {
        let cache = PipelineCache::new(ctx, &ArchiveConfig::default())?;
        let lib = ShaderLibrary::from_source(ctx, "nxkit_blit", MSL)?;
        let pipe = |key, fs| cache.render(key, &RenderPipelineDesc::new((&lib, "blit_vs"), Some((&lib, fs)), &[PRESENT_FORMAT]));
        Ok(Self {
            ring: FrameRing::new(ctx)?,
            plain: pipe(1, "blit_plain")?,
            srgb: pipe(2, "blit_srgb")?,
            table: ArgumentTable::new(ctx, 1, 1)?,
        })
    }

    /// Draws `src` into `drawable` and presents it at `at` (seconds on the `CACurrentMediaTime` clock).
    pub(crate) fn present(&mut self, src: &Texture, crop: nxkit_nvn::backend::Rect, drawable: Drawable, at: f64) -> Result<()> {
        let mut frame = self.ring.begin()?;
        let words: Vec<u8> = [crop.x, crop.y, crop.width, crop.height].iter().flat_map(|v| v.to_le_bytes()).collect();
        let crop_addr = frame.scratch().push(&words)?;
        let target = drawable.texture().clone();
        let pipeline = match src.desc().format {
            Format::Rgba8UnormSrgb | Format::Bgra8UnormSrgb => self.srgb.clone(),
            _ => self.plain.clone(),
        };
        let desc = RenderPassDesc {
            colors: vec![Some(ColorAttachment { depth_plane: 0,
                texture: target,
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
            // The game rendered the texture in an earlier command buffer of the same queue.
            pass.encode(&RenderCommand::WaitForEarlierPasses)?;
            pass.encode(&RenderCommand::SetPipeline(pipeline))?;
            pass.encode(&RenderCommand::SetTexture {
                index: 0,
                texture: src.clone(),
            })?;
            pass.encode(&RenderCommand::SetBuffer { index: 0, gpu_addr: crop_addr })?;
            pass.encode(&RenderCommand::Draw {
                primitive: Primitive::Triangle,
                first_vertex: 0,
                vertex_count: 3,
                instance_count: 1,
                base_instance: 0,
            })?;
            pass.end();
        }
        frame.present_at(drawable, at);
        frame.submit()?;
        Ok(())
    }
}
