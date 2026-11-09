//! Visibility results: the samples of draws counted into a buffer (the GPU counters of NVN). Needs a Metal 4 GPU;
//! without one the tests print a message and pass.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use nxkit_metal::{
    ArchiveConfig, ArgumentTable, ColorAttachment, Context, DataBuffer, Format, FrameRing,
    LoadAction, PipelineCache, Primitive, RenderCommand, RenderPassDesc, RenderPipelineDesc,
    ShaderLibrary, StoreAction, Texture, TextureDesc,
};

const MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;
struct V { float4 pos [[position]]; };
vertex V vs(uint id [[vertex_id]]) {
    float2 p = float2((id << 1) & 2, id & 2);
    V v; v.pos = float4(p * 2.0 - 1.0, 0.0, 1.0); return v;
}
fragment float4 fs(V in [[stage_in]]) { return float4(1.0, 0.0, 0.0, 1.0); }
"#;

fn ctx() -> Option<Context> {
    match Context::system_default() {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!("skipped: no usable Metal 4 device ({e})");
            None
        }
    }
}

fn word(b: &DataBuffer, i: u64) -> u64 {
    let mut v = [0u8; 8];
    b.read(i * 8, &mut v).unwrap();
    u64::from_le_bytes(v)
}

/// A full-screen triangle of `instances` instances.
fn triangle(instances: u32) -> RenderCommand {
    RenderCommand::Draw {
        primitive: Primitive::Triangle,
        first_vertex: 0,
        vertex_count: 3,
        instance_count: instances,
        base_instance: 0,
    }
}

fn count(at: Option<u64>) -> RenderCommand {
    RenderCommand::SetVisibility {
        counting: at.is_some(),
        offset: at.unwrap_or(0),
    }
}

#[test]
fn counted_draws_add_their_samples_at_their_offset_across_passes() {
    let Some(ctx) = ctx() else { return };
    let lib = ShaderLibrary::from_source(&ctx, "vis", MSL).unwrap();
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let pipeline = cache
        .render(
            31,
            &RenderPipelineDesc::new((&lib, "vs"), Some((&lib, "fs")), &[Format::Rgba8Unorm]),
        )
        .unwrap();
    let mut td = TextureDesc::render_target_2d(16, 16, Format::Rgba8Unorm);
    td.storage = nxkit_metal::Storage::Shared;
    let target = Texture::new(&ctx, td).unwrap();
    let desc = RenderPassDesc {
        colors: vec![Some(ColorAttachment {
            depth_plane: 0,
            texture: target,
            load: LoadAction::Clear,
            store: StoreAction::Store,
            clear: [0.0; 4],
            memoryless: false,
        })],
        depth: None,
        stencil: None,
    };
    let (vt, ft) = (
        ArgumentTable::new(&ctx, 1, 1).unwrap(),
        ArgumentTable::new(&ctx, 1, 1).unwrap(),
    );
    let vis = DataBuffer::new(&ctx, 32).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    let cb = frame.commands();
    {
        let mut pass = cb
            .begin_render_pass_counted(&desc, &vt, &ft, Some(&vis))
            .unwrap();
        pass.encode(&RenderCommand::SetPipeline(pipeline.clone()))
            .unwrap();
        pass.encode(&count(Some(0))).unwrap();
        pass.encode(&triangle(1)).unwrap();
        // Not counted.
        pass.encode(&count(None)).unwrap();
        pass.encode(&triangle(1)).unwrap();
        // No instance: no sample.
        pass.encode(&count(Some(8))).unwrap();
        pass.encode(&triangle(0)).unwrap();
        // Outside the buffer, or not aligned.
        assert!(pass.encode(&count(Some(32))).is_err());
        assert!(pass.encode(&count(Some(4))).is_err());
        pass.end();
    }
    {
        let mut pass = cb
            .begin_render_pass_counted(&desc, &vt, &ft, Some(&vis))
            .unwrap();
        pass.encode(&RenderCommand::SetPipeline(pipeline)).unwrap();
        pass.encode(&count(Some(16))).unwrap();
        pass.encode(&triangle(2)).unwrap();
        pass.end();
    }
    frame.submit().unwrap().wait().unwrap();
    assert_eq!(word(&vis, 0), 256);
    assert_eq!(word(&vis, 1), 0);
    assert_eq!(word(&vis, 2), 512);
    assert_eq!(word(&vis, 3), 0);
}

#[test]
fn a_pass_without_a_visibility_buffer_refuses_to_count() {
    let Some(ctx) = ctx() else { return };
    let mut td = TextureDesc::render_target_2d(16, 16, Format::Rgba8Unorm);
    td.storage = nxkit_metal::Storage::Shared;
    let desc = RenderPassDesc {
        colors: vec![Some(ColorAttachment {
            depth_plane: 0,
            texture: Texture::new(&ctx, td).unwrap(),
            load: LoadAction::Clear,
            store: StoreAction::Store,
            clear: [0.0; 4],
            memoryless: false,
        })],
        depth: None,
        stencil: None,
    };
    let table = ArgumentTable::new(&ctx, 1, 1).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    let mut pass = frame.commands().begin_render_pass(&desc, &table).unwrap();
    assert!(pass.encode(&count(Some(0))).is_err());
    assert!(pass.encode(&count(None)).is_ok());
    pass.end();
    frame.submit().unwrap().wait().unwrap();
}
