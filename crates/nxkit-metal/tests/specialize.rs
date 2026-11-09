//! Render pipelines specialized from an unspecialized pipeline of their pair of functions, on a real Metal 4 GPU.
//! Without a device the tests print a message and pass.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use nxkit_metal::{
    ArchiveConfig, ArgumentTable, ColorAttachment, ColorTarget, Context, Format, FrameRing,
    LoadAction, PipelineCache, Primitive, RenderCommand, RenderPassDesc, RenderPipeline,
    RenderPipelineDesc, ShaderLibrary, Source, StoreAction, Texture, TextureDesc,
};

const DRAW_MSL: &str = r#"
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

fn bases() -> ArchiveConfig {
    ArchiveConfig {
        compile_bases: true,
        ..Default::default()
    }
}

/// Clears a 16x16 target to blue, draws a full-screen triangle with `pipe` and reads the pixels back.
fn draw(ctx: &Context, pipe: RenderPipeline) -> Vec<u8> {
    let mut d = TextureDesc::render_target_2d(16, 16, Format::Rgba8Unorm);
    d.storage = nxkit_metal::Storage::Shared;
    let target = Texture::new(ctx, d).unwrap();
    let table = ArgumentTable::new(ctx, 1, 1).unwrap();
    let mut ring = FrameRing::new(ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    let pass_desc = RenderPassDesc {
        colors: vec![Some(ColorAttachment {
            depth_plane: 0,
            texture: target.clone(),
            load: LoadAction::Clear,
            store: StoreAction::Store,
            clear: [0.0, 0.0, 1.0, 1.0],
            memoryless: false,
        })],
        depth: None,
        stencil: None,
    };
    {
        let mut pass = frame
            .commands()
            .begin_render_pass(&pass_desc, &table)
            .unwrap();
        pass.encode(&RenderCommand::SetPipeline(pipe)).unwrap();
        pass.encode(&RenderCommand::Draw {
            primitive: Primitive::Triangle,
            first_vertex: 0,
            vertex_count: 3,
            instance_count: 1,
            base_instance: 0,
        })
        .unwrap();
        pass.end();
    }
    frame.submit().unwrap().wait().unwrap();
    let mut v = vec![0u8; 16 * 16 * 4];
    target.read(16 * 4, &mut v).unwrap();
    v
}

#[test]
fn a_pair_compiles_its_base_once_and_specializes_each_colour_state() {
    let Some(ctx) = ctx() else { return };
    let cache = PipelineCache::new(&ctx, &bases()).unwrap();
    let lib = ShaderLibrary::from_source(&ctx, "draw_b", DRAW_MSL).unwrap();
    let desc = |f: Format| RenderPipelineDesc::new((&lib, "vs"), Some((&lib, "fs")), &[f]);
    let (_, a) = cache.render_traced(1, &desc(Format::Rgba8Unorm)).unwrap();
    let (_, b) = cache.render_traced(2, &desc(Format::Bgra8Unorm)).unwrap();
    assert!(matches!(a, Source::Specialized(_)), "{a:?}");
    assert!(matches!(b, Source::Specialized(_)), "{b:?}");
    let s = cache.base_stats();
    assert_eq!((s.found, s.compiled, s.specialized, s.failed), (0, 1, 2, 0));
    // the cache keeps the specialized pipeline under its key
    assert_eq!(
        cache.render_traced(1, &desc(Format::Rgba8Unorm)).unwrap().1,
        Source::Cached
    );
}

#[test]
fn each_state_beyond_the_colour_attachments_has_a_base_of_its_own() {
    let Some(ctx) = ctx() else { return };
    let cache = PipelineCache::new(&ctx, &bases()).unwrap();
    let lib = ShaderLibrary::from_source(&ctx, "draw_b", DRAW_MSL).unwrap();
    let desc = |f: Format, samples: u32| {
        let mut d = RenderPipelineDesc::new((&lib, "vs"), Some((&lib, "fs")), &[f]);
        d.sample_count = samples;
        d
    };
    for (key, f, samples) in [
        (1, Format::Rgba8Unorm, 4),
        (2, Format::Bgra8Unorm, 4),
        (3, Format::Rgba8Unorm, 1),
    ] {
        let (_, s) = cache.render_traced(key, &desc(f, samples)).unwrap();
        assert!(matches!(s, Source::Specialized(_)), "{key}: {s:?}");
    }
    let s = cache.base_stats();
    assert_eq!((s.compiled, s.specialized), (2, 3), "{s:?}");
}

#[test]
fn a_depth_only_pipeline_is_compiled() {
    let Some(ctx) = ctx() else { return };
    let cache = PipelineCache::new(&ctx, &bases()).unwrap();
    let lib = ShaderLibrary::from_source(&ctx, "draw_b", DRAW_MSL).unwrap();
    let d = RenderPipelineDesc::new((&lib, "vs"), None, &[]);
    let (_, s) = cache.render_traced(1, &d).unwrap();
    assert!(matches!(s, Source::Compiled(_)), "{s:?}");
    assert_eq!(cache.base_stats().compiled, 0);
}

#[test]
fn without_compiled_bases_a_pair_the_archive_lacks_compiles() {
    let Some(ctx) = ctx() else { return };
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let lib = ShaderLibrary::from_source(&ctx, "draw_b", DRAW_MSL).unwrap();
    let d = RenderPipelineDesc::new((&lib, "vs"), Some((&lib, "fs")), &[Format::Rgba8Unorm]);
    let (_, s) = cache.render_traced(1, &d).unwrap();
    assert!(matches!(s, Source::Compiled(_)), "{s:?}");
    assert_eq!(cache.base_stats().missing, 1);
}

#[test]
fn a_specialized_pipeline_draws_with_its_format_and_write_mask() {
    let Some(ctx) = ctx() else { return };
    let cache = PipelineCache::new(&ctx, &bases()).unwrap();
    let lib = ShaderLibrary::from_source(&ctx, "draw_b", DRAW_MSL).unwrap();
    let mut d = RenderPipelineDesc::new((&lib, "vs"), Some((&lib, "fs")), &[Format::Rgba8Unorm]);
    let (all, s) = cache.render_traced(1, &d).unwrap();
    assert!(matches!(s, Source::Specialized(_)), "{s:?}");
    assert!(draw(&ctx, all).chunks(4).all(|p| p == [255, 0, 0, 255]));
    // red and alpha only: the blue of the clear stays
    d.colors = vec![Some(ColorTarget {
        write_mask: 0b1001,
        ..ColorTarget::new(Format::Rgba8Unorm)
    })];
    let (masked, s) = cache.render_traced(2, &d).unwrap();
    assert!(matches!(s, Source::Specialized(_)), "{s:?}");
    assert!(
        draw(&ctx, masked)
            .chunks(4)
            .all(|p| p == [255, 0, 255, 255])
    );
}

/// The build's archive holds unspecialized pipelines; one written by a cache that compiled its base stands in for it.
#[test]
fn an_archived_base_is_specialized_without_a_compile() {
    let Some(ctx) = ctx() else { return };
    let dir = std::env::temp_dir().join(format!("nxkit-specialize-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let archive = dir.join("bases.mtl4archive");
    let lib = ShaderLibrary::from_source(&ctx, "draw_b", DRAW_MSL).unwrap();
    let desc = |f: Format| RenderPipelineDesc::new((&lib, "vs"), Some((&lib, "fs")), &[f]);
    let first = PipelineCache::new(
        &ctx,
        &ArchiveConfig {
            collect: true,
            compile_bases: true,
            ..Default::default()
        },
    )
    .unwrap();
    first.render(1, &desc(Format::Rgba8Unorm)).unwrap();
    assert_eq!(first.base_stats().compiled, 1);
    first.save_archive(&archive).unwrap();
    let second = PipelineCache::new(
        &ctx,
        &ArchiveConfig {
            load: Some(archive),
            ..Default::default()
        },
    )
    .unwrap();
    let (_, s) = second.render_traced(1, &desc(Format::Bgra8Unorm)).unwrap();
    assert!(matches!(s, Source::Specialized(_)), "{s:?}");
    let b = second.base_stats();
    assert_eq!((b.found, b.compiled), (1, 0), "{b:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}
