//! The tests of the render pass encoding.

use super::*;
use nxkit_metal::{
    ArchiveConfig, Format, FrameRing, PipelineCache, Primitive, RenderPipelineDesc, ShaderLibrary,
    Texture, TextureDesc,
};

#[test]
fn inactive_aliases_leave_the_original_output_locations_empty() {
    let colors = [
        (0, TexId(1), false),
        (1, TexId(2), false),
        (2, TexId(2), false),
        (3, TexId(3), false),
        (4, TexId(3), false),
    ];
    assert_eq!(distinct_color_slots(&colors, 0b01011).unwrap(), 0b01011);
    assert_eq!(distinct_color_slots(&colors, 0b10101).unwrap(), 0b10101);
    assert_eq!(
        distinct_color_slots(&[(1, TexId(2), false), (2, TexId(2), true)], 0).unwrap(),
        4
    );
    assert!(distinct_color_slots(&colors, 0b00110).is_err());
}

#[test]
fn an_inactive_alias_cannot_overwrite_a_rendered_color() {
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    let selected = distinct_color_slots(&[(0, TexId(1), false), (1, TexId(1), false)], 1).unwrap();
    let lib = ShaderLibrary::from_source(
        &ctx,
        "alias-output",
        r#"
#include <metal_stdlib>
using namespace metal;
struct V { float4 pos [[position]]; };
vertex V vs(uint id [[vertex_id]]) {
float2 p = float2((id << 1) & 2, id & 2);
return {float4(p * 2 - 1, 0, 1)};
}
struct Outputs { float4 a [[color(0)]]; float4 b [[color(1)]]; };
fragment Outputs fs() { return {float4(1,0,0,1), float4(0)}; }
"#,
    )
    .unwrap();
    let mut texture_desc = TextureDesc::render_target_2d(16, 16, Format::Rgba8Unorm);
    texture_desc.storage = Storage::Shared;
    let texture = Texture::new(&ctx, texture_desc).unwrap();
    let mut pipeline_desc =
        RenderPipelineDesc::new((&lib, "vs"), Some((&lib, "fs")), &[Format::Rgba8Unorm; 2]);
    pipeline_desc.colors[1].as_mut().unwrap().write_mask = 0;
    let mut colors = vec![
        Some(ColorAttachment {
            depth_plane: 0,
            texture: texture.clone(),
            load: MLoad::Clear,
            store: MStore::Store,
            clear: [0.0; 4],
            memoryless: false
        });
        2
    ];
    for (slot, color) in colors.iter_mut().enumerate() {
        if selected & (1 << slot) == 0 {
            *color = None;
            pipeline_desc.colors[slot] = None;
        }
    }
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let pipeline = cache.render(1, &pipeline_desc).unwrap();
    let table = ArgumentTable::new(&ctx, 1, 1).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    let mut pass = frame
        .commands()
        .begin_render_pass(
            &RenderPassDesc {
                colors,
                depth: None,
                stencil: None,
            },
            &table,
        )
        .unwrap();
    pass.encode(&RenderCommand::SetPipeline(pipeline)).unwrap();
    pass.encode(&RenderCommand::Draw {
        primitive: Primitive::Triangle,
        first_vertex: 0,
        vertex_count: 3,
        instance_count: 1,
        base_instance: 0,
    })
    .unwrap();
    pass.end();
    frame.submit().unwrap().wait().unwrap();
    let mut pixels = vec![0; 16 * 16 * 4];
    texture.read(16 * 4, &mut pixels).unwrap();
    assert!(
        pixels
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| *p == [255, 0, 0, 255])
    );
}
