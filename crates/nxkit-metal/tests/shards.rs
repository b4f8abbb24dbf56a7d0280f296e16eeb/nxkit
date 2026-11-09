//! Pipeline archive shards on a real Metal 4 GPU. Without a device the tests print a message and pass.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use nxkit_abi::shards::{INDEX_FILE, IndexBuilder};
use nxkit_metal::{
    ArchiveConfig, Context, Format, PipelineCache, RenderPipelineDesc, ShaderLibrary, Source,
    mem_stats,
};

const ADD_MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;
kernel void add_words(device uint* d [[buffer(0)]], uint i [[thread_position_in_grid]]) { d[i] += 1; }
"#;

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

/// The memory counters are process-wide: one test at a time.
static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn ctx() -> Option<Context> {
    match Context::system_default() {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!("skipped: no usable Metal 4 device ({e})");
            None
        }
    }
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nxkit-shards-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Writes the archive of what `build` compiles as `file` in `dir`.
fn shard(ctx: &Context, dir: &Path, file: &str, build: impl Fn(&PipelineCache)) {
    let c = PipelineCache::new(
        ctx,
        &ArchiveConfig {
            collect: true,
            ..Default::default()
        },
    )
    .unwrap();
    build(&c);
    c.save_archive(&dir.join(file)).unwrap();
}

#[test]
fn a_shard_is_opened_by_the_first_lookup_and_found_through_the_index() {
    let _one = ONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(ctx) = ctx() else { return };
    let dir = scratch("route");
    let draw = ShaderLibrary::from_source(&ctx, "draw_b", DRAW_MSL).unwrap();
    let add = ShaderLibrary::from_source(&ctx, "add_b", ADD_MSL).unwrap();
    let desc =
        || RenderPipelineDesc::new((&draw, "vs"), Some((&draw, "fs")), &[Format::Rgba8Unorm]);
    shard(&ctx, &dir, "r.mtl4archive", |c| {
        c.render(1, &desc()).unwrap();
    });
    shard(&ctx, &dir, "c.mtl4archive", |c| {
        c.compute(1, &add, "add_words").unwrap();
    });
    let mut ix = IndexBuilder::new();
    let (r, c) = (ix.shard("r.mtl4archive"), ix.shard("c.mtl4archive"));
    ix.render(("draw_b", "vs"), ("draw_b", "fs"), r);
    ix.compute(("add_b", "add_words"), c);
    std::fs::write(dir.join(INDEX_FILE), ix.encode()).unwrap();

    let cache = PipelineCache::new(
        &ctx,
        &ArchiveConfig {
            shards: Some(dir.clone()),
            open_shards: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(cache.has_archive());
    // nothing is open before the first lookup
    assert_eq!(cache.shard_stats(), Some((2, 2, 0, 0)));
    assert_eq!(mem_stats().archive_shards, 0);
    assert_eq!(cache.render_traced(1, &desc()).unwrap().1, Source::Archive);
    assert_eq!(cache.shard_stats(), Some((2, 2, 1, 1)));
    assert_eq!(mem_stats().archive_shards, 1);
    assert!(mem_stats().archive_shard_bytes > 0);
    // the compute pipeline is in the other shard: with room for one, the render shard closes
    cache.compute(2, &add, "add_words").unwrap();
    assert_eq!(cache.shard_stats(), Some((2, 2, 1, 2)));
    // the render shard is asked again for a pipeline of another key: it reopens
    assert_eq!(cache.render_traced(3, &desc()).unwrap().1, Source::Archive);
    assert_eq!(cache.shard_stats(), Some((2, 2, 1, 3)));
    assert_eq!(mem_stats().archive_shards, 1);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_pipeline_the_index_lacks_is_compiled_and_opens_no_shard() {
    let _one = ONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(ctx) = ctx() else { return };
    let dir = scratch("miss");
    let draw = ShaderLibrary::from_source(&ctx, "draw_b", DRAW_MSL).unwrap();
    let mut ix = IndexBuilder::new();
    let s = ix.shard("none.mtl4archive");
    ix.render(("other_b", "vs"), ("other_b", "fs"), s);
    std::fs::write(dir.join(INDEX_FILE), ix.encode()).unwrap();
    let cache = PipelineCache::new(
        &ctx,
        &ArchiveConfig {
            shards: Some(dir.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    let d = RenderPipelineDesc::new((&draw, "vs"), Some((&draw, "fs")), &[Format::Rgba8Unorm]);
    assert!(matches!(
        cache.render_traced(1, &d).unwrap().1,
        Source::Compiled(_)
    ));
    assert_eq!(cache.shard_stats(), Some((1, 1, 0, 0)));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_missing_shard_file_falls_back_to_compiling() {
    let _one = ONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(ctx) = ctx() else { return };
    let dir = scratch("gone");
    let draw = ShaderLibrary::from_source(&ctx, "draw_b", DRAW_MSL).unwrap();
    let mut ix = IndexBuilder::new();
    let s = ix.shard("gone.mtl4archive");
    ix.render(("draw_b", "vs"), ("draw_b", "fs"), s);
    std::fs::write(dir.join(INDEX_FILE), ix.encode()).unwrap();
    let cache = PipelineCache::new(
        &ctx,
        &ArchiveConfig {
            shards: Some(dir.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    let d = RenderPipelineDesc::new((&draw, "vs"), Some((&draw, "fs")), &[Format::Rgba8Unorm]);
    assert!(matches!(
        cache.render_traced(1, &d).unwrap().1,
        Source::Compiled(_)
    ));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_folder_without_an_index_leaves_the_single_archive_in_use() {
    let _one = ONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(ctx) = ctx() else { return };
    let dir = scratch("old");
    let draw = ShaderLibrary::from_source(&ctx, "draw_b", DRAW_MSL).unwrap();
    let desc =
        || RenderPipelineDesc::new((&draw, "vs"), Some((&draw, "fs")), &[Format::Rgba8Unorm]);
    shard(&ctx, &dir, "all.mtl4archive", |c| {
        c.render(1, &desc()).unwrap();
    });
    let cache = PipelineCache::new(
        &ctx,
        &ArchiveConfig {
            load: Some(dir.join("all.mtl4archive")),
            shards: Some(dir.join("no-such-folder")),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(cache.has_archive());
    assert_eq!(cache.shard_stats(), None);
    assert_eq!(cache.render_traced(1, &desc()).unwrap().1, Source::Archive);
    // the single archive is counted as one open archive with its file size
    assert!(mem_stats().archive_shard_bytes > 0);
    drop(cache);
    std::fs::remove_dir_all(&dir).unwrap();
}
