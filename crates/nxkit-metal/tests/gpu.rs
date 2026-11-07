//! Tests that need a real Metal 4 GPU. Without a device they print a message and pass.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use nxkit_metal::{
    ArchiveConfig, ArgumentTable, ColorAttachment, ComputeCommand, Context, DeswizzleParams,
    Deswizzler, Format, FrameRing, GobOrder, GuestMemory, LoadAction, MetalError, PipelineCache,
    Presenter, Primitive, RenderCommand, RenderPassDesc, RenderPipelineDesc, ShaderLibrary,
    StoreAction, Texture, TextureDesc,
};

const MIB: u64 = 1024 * 1024;

fn ctx() -> Option<Context> {
    match Context::system_default() {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!("skipped: no usable Metal 4 device ({e})");
            None
        }
    }
}

fn guest(ctx: &Context, len: u64) -> Arc<GuestMemory> {
    Arc::new(GuestMemory::allocate(ctx, len).unwrap())
}

fn pattern(n: usize, seed: u32) -> Vec<u8> {
    let mut s = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s >> 8) as u8
        })
        .collect()
}

const ADD_MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;
struct P { ulong src; ulong dst; uint count; uint add; };
kernel void add_words(constant P& p [[buffer(0)]], uint i [[thread_position_in_grid]]) {
    if (i >= p.count) { return; }
    device const uint* src = (device const uint*)p.src;
    device uint* dst = (device uint*)p.dst;
    dst[i] = src[i] + p.add;
}
"#;

fn params(src: u64, dst: u64, count: u32, add: u32) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend(src.to_le_bytes());
    b.extend(dst.to_le_bytes());
    b.extend(count.to_le_bytes());
    b.extend(add.to_le_bytes());
    b
}

#[test]
fn guest_memory_round_trip_and_address_map() {
    let Some(ctx) = ctx() else { return };
    let g = guest(&ctx, 4 * MIB);
    let data = pattern(4096, 1);
    g.write(MIB, &data).unwrap();
    let mut out = vec![0u8; 4096];
    g.read(MIB, &mut out).unwrap();
    assert_eq!(out, data);
    let map = g.address_map();
    let base = g.gpu_address_of_offset(0, 1).unwrap();
    assert_eq!(g.gpu_address(g.host_base() + MIB, 4).unwrap(), base + MIB);
    assert_eq!(map.gpu_address(g.host_base() + 100, 1).unwrap(), base + 100);
    assert!(matches!(
        g.gpu_address(g.host_base() + 4 * MIB, 1),
        Err(MetalError::OutOfRange { .. })
    ));
    assert!(g.write(4 * MIB - 2, &[0; 4]).is_err());
}

#[test]
fn guest_memory_wraps_caller_memory() {
    let Some(ctx) = ctx() else { return };
    let layout = std::alloc::Layout::from_size_align(2 * MIB as usize, 16 * 1024).unwrap();
    // SAFETY: the layout has a non-zero size.
    let p = unsafe { std::alloc::alloc_zeroed(layout) };
    let nn = std::ptr::NonNull::new(p).unwrap();
    {
        // SAFETY: the region is page aligned, `2 MiB` long, and freed after the wrapper is dropped.
        let g = unsafe { GuestMemory::from_raw(&ctx, nn, 2 * MIB) }.unwrap();
        g.write(8, &[7, 8, 9]).unwrap();
        // SAFETY: `p` is valid for 2 MiB and nothing else writes to it now.
        let seen = unsafe { std::slice::from_raw_parts(p.add(8), 3) };
        assert_eq!(seen, &[7, 8, 9]);
        assert_eq!(g.host_base(), p as u64);
    }
    // SAFETY: allocated above with the same layout and no longer used.
    unsafe { std::alloc::dealloc(p, layout) };
}

#[test]
fn guest_memory_rounds_up_to_pages_and_rejects_empty() {
    let Some(ctx) = ctx() else { return };
    assert_eq!(
        GuestMemory::allocate(&ctx, 100).unwrap().len(),
        nxkit_metal::PAGE_SIZE
    );
    assert!(GuestMemory::allocate(&ctx, 0).is_err());
}

#[test]
fn compute_reads_and_writes_guest_memory_through_addresses() {
    let Some(ctx) = ctx() else { return };
    let g = guest(&ctx, 8 * MIB);
    let n = 4096u32;
    let input: Vec<u32> = (0..n).map(|i| i * 3 + 1).collect();
    let bytes: Vec<u8> = input.iter().flat_map(|v| v.to_le_bytes()).collect();
    let (src_off, dst_off) = (MIB, 5 * MIB + 64);
    g.write(src_off, &bytes).unwrap();

    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let lib = ShaderLibrary::from_source(&ctx, "add", ADD_MSL).unwrap();
    let pipe = cache.compute(10, &lib, "add_words").unwrap();
    let table = ArgumentTable::new(&ctx, 1, 0).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    let src = g.gpu_address_of_offset(src_off, 4 * u64::from(n)).unwrap();
    let dst = g.gpu_address_of_offset(dst_off, 4 * u64::from(n)).unwrap();
    let p = frame.scratch().push(&params(src, dst, n, 1000)).unwrap();
    {
        let mut pass = frame.commands().begin_compute_pass(&table).unwrap();
        pass.encode(&ComputeCommand::SetPipeline(pipe)).unwrap();
        pass.encode(&ComputeCommand::SetBuffer {
            index: 0,
            gpu_addr: p,
        })
        .unwrap();
        pass.encode(&ComputeCommand::DispatchThreads {
            grid: [n, 1, 1],
            threads: [64, 1, 1],
        })
        .unwrap();
        pass.end();
    }
    let stats = frame.submit().unwrap().wait().unwrap();
    assert!(stats.gpu_ns > 0);
    let mut out = vec![0u8; bytes.len()];
    g.read(dst_off, &mut out).unwrap();
    let got: Vec<u32> = out
        .chunks(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    let want: Vec<u32> = input.iter().map(|v| v + 1000).collect();
    assert_eq!(got, want);
}

#[test]
fn frames_in_flight_reuse_slots_and_record_metrics() {
    let Some(ctx) = ctx() else { return };
    let g = guest(&ctx, 2 * MIB);
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let lib = ShaderLibrary::from_source(&ctx, "add", ADD_MSL).unwrap();
    let pipe = cache.compute(10, &lib, "add_words").unwrap();
    let table = ArgumentTable::new(&ctx, 1, 0).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    let addr = g.gpu_address_of_offset(0, 1024).unwrap();
    let mut last = None;
    for i in 0..8u32 {
        let mut frame = ring.begin().unwrap();
        let p = frame
            .scratch()
            .push(&params(addr, addr + 4096, 16, i))
            .unwrap();
        let mut pass = frame.commands().begin_compute_pass(&table).unwrap();
        pass.encode(&ComputeCommand::SetPipeline(pipe.clone()))
            .unwrap();
        pass.encode(&ComputeCommand::SetBuffer {
            index: 0,
            gpu_addr: p,
        })
        .unwrap();
        pass.encode(&ComputeCommand::DispatchThreads {
            grid: [16, 1, 1],
            threads: [16, 1, 1],
        })
        .unwrap();
        pass.end();
        last = Some(frame.submit().unwrap());
    }
    last.unwrap().wait().unwrap();
    let mut seen = 0;
    while seen < 3 {
        let f = ring.begin().unwrap();
        f.submit().unwrap().wait().unwrap();
        seen += 1;
    }
    let m = ring.metrics().snapshot();
    assert!(m.completed >= 8, "{m:?}");
    assert_eq!(m.failed, 0);
    assert!(m.cpu_encode_total_ns > 0);
}

fn gob_offset(x: u32, y: u32, order: GobOrder) -> u32 {
    match order {
        GobOrder::Tegra => {
            (x % 64 / 32) * 256 + (y % 8 / 2) * 64 + (x % 32 / 16) * 32 + (y % 2) * 16 + x % 16
        }
        GobOrder::Switch2 => {
            (x % 64 / 32) * 256
                + (y % 8 / 4) * 128
                + (x % 32 / 16) * 64
                + (y % 4 / 2) * 32
                + (y % 2) * 16
                + x % 16
        }
    }
}

// An independent reference, written from the layout rules, not copied from the kernel.
fn cpu_deswizzle(src: &[u8], p: &DeswizzleParams) -> Vec<u8> {
    let bpb = p.bytes_per_block;
    let pitch = if p.pitch == 0 { p.width * bpb } else { p.pitch };
    let gobs_x = pitch.div_ceil(64);
    let bh = p.block_height;
    let mut out = vec![0u8; (p.width * p.height * bpb) as usize];
    for y in 0..p.height {
        for x in 0..p.width {
            let xb = x * bpb;
            let off = (y / (8 * bh)) as u64 * u64::from(gobs_x) * 512 * u64::from(bh)
                + u64::from(xb / 64) * 512 * u64::from(bh)
                + u64::from((y % (8 * bh)) / 8) * 512
                + u64::from(gob_offset(xb, y, p.order));
            let o = ((y * p.width + x) * bpb) as usize;
            out[o..o + bpb as usize]
                .copy_from_slice(&src[off as usize..off as usize + bpb as usize]);
        }
    }
    out
}

fn run_deswizzle(ctx: &Context, p: DeswizzleParams) {
    let g = guest(ctx, 8 * MIB);
    let store = p.storage_bytes() as usize;
    let data = pattern(store, p.width ^ p.height ^ p.bytes_per_block);
    let off = 16 * 1024;
    g.write(off, &data).unwrap();
    let fmt = Format::uint_for_block_bytes(p.bytes_per_block).unwrap();
    let tex = Texture::new(ctx, TextureDesc::compute_2d(p.width, p.height, fmt)).unwrap();
    let cache = PipelineCache::new(ctx, &ArchiveConfig::default()).unwrap();
    let ds = Deswizzler::new(ctx, &cache).unwrap();
    let mut ring = FrameRing::new(ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    let scratch = frame.scratch();
    let mut pass = frame.commands().begin_compute_pass(ds.table()).unwrap();
    ds.encode(&mut pass, &scratch, &g, off, &tex, &p).unwrap();
    pass.end();
    frame.submit().unwrap().wait().unwrap();
    let bpr = (p.width * p.bytes_per_block) as usize;
    let mut got = vec![0u8; bpr * p.height as usize];
    tex.read(bpr, &mut got).unwrap();
    assert_eq!(got, cpu_deswizzle(&data, &p), "{p:?}");
}

#[test]
fn deswizzle_matches_cpu_reference() {
    let Some(ctx) = ctx() else { return };
    let cases = [
        (1, 100, 70, 1, 0, GobOrder::Tegra),
        (2, 33, 17, 2, 0, GobOrder::Tegra),
        (4, 256, 128, 4, 0, GobOrder::Tegra),
        (4, 130, 90, 16, 0, GobOrder::Tegra),
        (8, 64, 64, 8, 0, GobOrder::Tegra),
        (16, 60, 45, 2, 0, GobOrder::Tegra),
        (4, 100, 40, 2, 512, GobOrder::Tegra),
        (4, 128, 64, 4, 0, GobOrder::Switch2),
        (16, 31, 33, 1, 0, GobOrder::Switch2),
    ];
    for (bytes_per_block, width, height, block_height, pitch, order) in cases {
        run_deswizzle(
            &ctx,
            DeswizzleParams {
                bytes_per_block,
                width,
                height,
                block_height,
                pitch,
                order,
            },
        );
    }
}

#[test]
fn deswizzle_rejects_bad_params() {
    let Some(ctx) = ctx() else { return };
    let g = guest(&ctx, 2 * MIB);
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let ds = Deswizzler::new(&ctx, &cache).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    let scratch = frame.scratch();
    let tex = Texture::new(&ctx, TextureDesc::compute_2d(8, 8, Format::R32Uint)).unwrap();
    let mut pass = frame.commands().begin_compute_pass(ds.table()).unwrap();
    let ok = DeswizzleParams {
        bytes_per_block: 4,
        width: 8,
        height: 8,
        block_height: 1,
        pitch: 0,
        order: GobOrder::Tegra,
    };
    for bad in [
        DeswizzleParams {
            bytes_per_block: 3,
            ..ok
        },
        DeswizzleParams {
            block_height: 3,
            ..ok
        },
        DeswizzleParams { pitch: 4, ..ok },
        DeswizzleParams {
            bytes_per_block: 2,
            ..ok
        },
    ] {
        assert!(
            ds.encode(&mut pass, &scratch, &g, 0, &tex, &bad).is_err(),
            "{bad:?}"
        );
    }
    assert!(
        ds.encode(&mut pass, &scratch, &g, 3 * MIB, &tex, &ok)
            .is_err()
    );
    pass.end();
}

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

fn pixels(tex: &Texture, w: u32, h: u32) -> Vec<u8> {
    let mut v = vec![0u8; (w * h * 4) as usize];
    tex.read((w * 4) as usize, &mut v).unwrap();
    v
}

fn shared_target(ctx: &Context, w: u32, h: u32) -> Texture {
    let mut d = TextureDesc::render_target_2d(w, h, Format::Rgba8Unorm);
    d.storage = nxkit_metal::Storage::Shared;
    Texture::new(ctx, d).unwrap()
}

#[test]
fn render_clear_and_draw_then_copy_back_to_guest() {
    let Some(ctx) = ctx() else { return };
    let (w, h) = (64u32, 64u32);
    let g = guest(&ctx, 2 * MIB);
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let lib = ShaderLibrary::from_source(&ctx, "draw", DRAW_MSL).unwrap();
    let pipe = cache
        .render(
            20,
            &RenderPipelineDesc::new((&lib, "vs"), Some((&lib, "fs")), &[Format::Rgba8Unorm]),
        )
        .unwrap();
    let clear_only = shared_target(&ctx, w, h);
    let drawn = shared_target(&ctx, w, h);
    let table = ArgumentTable::new(&ctx, 1, 1).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    let att = |t: &Texture| RenderPassDesc {
        colors: vec![ColorAttachment {
            texture: t.clone(),
            load: LoadAction::Clear,
            store: StoreAction::Store,
            clear: [0.0, 0.0, 1.0, 1.0],
            memoryless: false,
        }],
        depth: None,
        stencil: None,
    };
    frame
        .commands()
        .begin_render_pass(&att(&clear_only), &table)
        .unwrap()
        .end();
    {
        let mut pass = frame
            .commands()
            .begin_render_pass(&att(&drawn), &table)
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
    {
        let mut pass = frame.commands().begin_compute_pass(&table).unwrap();
        pass.encode(&ComputeCommand::WaitForEarlierPasses).unwrap();
        pass.encode(&ComputeCommand::CopyTextureToGuest {
            texture: drawn.clone(),
            guest: g.clone(),
            offset: 4096,
            bytes_per_row: w * 4,
        })
        .unwrap();
        pass.end();
    }
    frame.submit().unwrap().wait().unwrap();
    assert!(
        pixels(&clear_only, w, h)
            .chunks(4)
            .all(|p| p == [0, 0, 255, 255])
    );
    let red = pixels(&drawn, w, h);
    assert!(red.chunks(4).all(|p| p == [255, 0, 0, 255]));
    let mut copy = vec![0u8; red.len()];
    g.read(4096, &mut copy).unwrap();
    assert_eq!(copy, red);
}

#[test]
fn memoryless_attachment_that_stores_is_refused() {
    let Some(ctx) = ctx() else { return };
    let mut d = TextureDesc::render_target_2d(16, 16, Format::Rgba8Unorm);
    d.storage = nxkit_metal::Storage::Memoryless;
    let t = Texture::new(&ctx, d).unwrap();
    let table = ArgumentTable::new(&ctx, 1, 1).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    let bad = RenderPassDesc {
        colors: vec![ColorAttachment {
            texture: t,
            load: LoadAction::Clear,
            store: StoreAction::Store,
            clear: [0.0; 4],
            memoryless: true,
        }],
        depth: None,
        stencil: None,
    };
    assert!(frame.commands().begin_render_pass(&bad, &table).is_err());
}

#[test]
fn pipeline_cache_counts_hits_and_archive_round_trips() {
    let Some(ctx) = ctx() else { return };
    let dir = std::env::temp_dir().join(format!("nxkit-metal-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let archive = dir.join("pipes.mtl4a");
    let lib = ShaderLibrary::from_source(&ctx, "add", ADD_MSL).unwrap();
    assert!(lib.function_names().contains(&"add_words".to_string()));
    let cache = PipelineCache::new(
        &ctx,
        &ArchiveConfig {
            load: None,
            collect: true,
        },
    )
    .unwrap();
    cache.compute(1, &lib, "add_words").unwrap();
    cache.compute(1, &lib, "add_words").unwrap();
    assert_eq!((cache.builds(), cache.hits()), (1, 1));
    cache.save_archive(&archive).unwrap();
    assert!(archive.exists());
    let again = PipelineCache::new(
        &ctx,
        &ArchiveConfig {
            load: Some(archive.clone()),
            collect: false,
        },
    )
    .unwrap();
    assert!(again.has_archive());
    again.compute(1, &lib, "add_words").unwrap();
    assert_eq!(again.builds(), 1);
    let missing = PipelineCache::new(
        &ctx,
        &ArchiveConfig {
            load: Some(dir.join("none")),
            collect: false,
        },
    )
    .unwrap();
    assert!(!missing.has_archive());
    assert!(cache.compute(2, &lib, "no_such_function").is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn offscreen_drawable_is_presented() {
    let Some(ctx) = ctx() else { return };
    let presenter = Presenter::offscreen(&ctx, 32, 32);
    let table = ArgumentTable::new(&ctx, 1, 1).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    for _ in 0..3 {
        let mut frame = ring.begin().unwrap();
        let drawable = presenter.acquire().unwrap();
        frame.commands().retain_drawable(&drawable);
        let desc = RenderPassDesc {
            colors: vec![ColorAttachment {
                texture: drawable.texture().clone(),
                load: LoadAction::Clear,
                store: StoreAction::Store,
                clear: [0.0, 1.0, 0.0, 1.0],
                memoryless: false,
            }],
            depth: None,
            stencil: None,
        };
        frame
            .commands()
            .begin_render_pass(&desc, &table)
            .unwrap()
            .end();
        frame.present(drawable);
        let stats = frame.submit().unwrap().wait().unwrap();
        assert!(!stats.failed);
    }
}

const BLEND_MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;
struct V { float4 pos [[position]]; };
vertex V vs(uint id [[vertex_id]]) {
    float2 p = float2((id << 1) & 2, id & 2);
    V v; v.pos = float4(p * 2.0 - 1.0, 0.0, 1.0); return v;
}
fragment float4 fs(V in [[stage_in]]) { return float4(1.0, 0.0, 0.0, 0.5); }
"#;

#[test]
fn blend_and_depth_state_change_the_result() {
    use nxkit_metal::{
        BlendState, ColorTarget, CompareFn, DepthAttachment, DepthStencilDesc, DepthStencilState,
    };
    let Some(ctx) = ctx() else { return };
    let (w, h) = (16u32, 16u32);
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let lib = ShaderLibrary::from_source(&ctx, "blend", BLEND_MSL).unwrap();
    let mut desc = RenderPipelineDesc::new((&lib, "vs"), Some((&lib, "fs")), &[Format::Rgba8Unorm]);
    desc.colors = vec![ColorTarget::new(Format::Rgba8Unorm).with_blend(BlendState::ALPHA)];
    let pipe = cache.render(30, &desc).unwrap();
    let blended = shared_target(&ctx, w, h);
    let rejected = shared_target(&ctx, w, h);
    let depth = Texture::new(
        &ctx,
        TextureDesc::render_target_2d(w, h, Format::Depth32Float),
    )
    .unwrap();
    let test = DepthStencilState::new(
        &ctx,
        &DepthStencilDesc {
            depth_compare: CompareFn::Less,
            depth_write: true,
            ..DepthStencilDesc::OFF
        },
    )
    .unwrap();
    let table = ArgumentTable::new(&ctx, 1, 1).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    let draw = RenderCommand::Draw {
        primitive: Primitive::Triangle,
        first_vertex: 0,
        vertex_count: 3,
        instance_count: 1,
        base_instance: 0,
    };
    let colors = |t: &Texture| {
        vec![ColorAttachment {
            texture: t.clone(),
            load: LoadAction::Clear,
            store: StoreAction::Store,
            clear: [0.0, 0.0, 1.0, 1.0],
            memoryless: false,
        }]
    };
    let mut pass = frame
        .commands()
        .begin_render_pass(
            &RenderPassDesc {
                colors: colors(&blended),
                ..Default::default()
            },
            &table,
        )
        .unwrap();
    pass.encode(&RenderCommand::SetPipeline(pipe.clone()))
        .unwrap();
    pass.encode(&draw).unwrap();
    pass.end();
    // The depth is cleared to 0, so a fragment at depth 0 fails the `Less` test.
    let mut pass = frame
        .commands()
        .begin_render_pass(
            &RenderPassDesc {
                colors: colors(&rejected),
                depth: Some(DepthAttachment {
                    texture: depth,
                    load: LoadAction::Clear,
                    store: StoreAction::DontCare,
                    clear: 0.0,
                    memoryless: false,
                }),
                stencil: None,
            },
            &table,
        )
        .unwrap();
    pass.encode(&RenderCommand::SetPipeline(pipe)).unwrap();
    pass.encode(&RenderCommand::SetDepthStencil(test)).unwrap();
    pass.encode(&draw).unwrap();
    pass.end();
    frame.submit().unwrap().wait().unwrap();
    let b = pixels(&blended, w, h);
    assert!(
        b.chunks(4)
            .all(|p| p[0].abs_diff(128) <= 1 && p[2].abs_diff(128) <= 1)
    );
    assert!(
        pixels(&rejected, w, h)
            .chunks(4)
            .all(|p| p == [0, 0, 255, 255])
    );
}

#[test]
fn explicit_barrier_orders_a_copy_after_a_draw_and_checks_stages() {
    use nxkit_metal::{Dependency, Stages, Use, Visibility};
    let Some(ctx) = ctx() else { return };
    let (w, h) = (32u32, 32u32);
    let g = guest(&ctx, 2 * MIB);
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let lib = ShaderLibrary::from_source(&ctx, "draw", DRAW_MSL).unwrap();
    let pipe = cache
        .render(
            40,
            &RenderPipelineDesc::new((&lib, "vs"), Some((&lib, "fs")), &[Format::Rgba8Unorm]),
        )
        .unwrap();
    let target = shared_target(&ctx, w, h);
    let table = ArgumentTable::new(&ctx, 1, 1).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    let desc = RenderPassDesc {
        colors: vec![ColorAttachment {
            texture: target.clone(),
            load: LoadAction::Clear,
            store: StoreAction::Store,
            clear: [0.0; 4],
            memoryless: false,
        }],
        ..Default::default()
    };
    let mut pass = frame.commands().begin_render_pass(&desc, &table).unwrap();
    let blit = Dependency::between(Use::CopyWrite, Use::CopyRead);
    assert!(pass.encode(&RenderCommand::Depend(blit)).is_err());
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
    let mut pass = frame.commands().begin_compute_pass(&table).unwrap();
    let dep = Dependency::between(Use::AttachmentWrite, Use::CopyRead);
    assert_eq!(
        (dep.after, dep.before, dep.visibility),
        (Stages::FRAGMENT, Stages::BLIT, Visibility::Device)
    );
    pass.encode(&ComputeCommand::Depend(dep)).unwrap();
    assert!(
        pass.encode(&ComputeCommand::Depend(Dependency::between(
            Use::AttachmentWrite,
            Use::FragmentRead
        )))
        .is_err()
    );
    pass.encode(&ComputeCommand::CopyTextureToGuest {
        texture: target,
        guest: g.clone(),
        offset: 0,
        bytes_per_row: w * 4,
    })
    .unwrap();
    pass.end();
    frame.submit().unwrap().wait().unwrap();
    let mut out = vec![0u8; (w * h * 4) as usize];
    g.read(0, &mut out).unwrap();
    assert!(out.chunks(4).all(|p| p == [255, 0, 0, 255]));
}

#[test]
fn an_indirect_command_buffer_draws_in_a_pass_and_refuses_inherited_state() {
    use nxkit_metal::{IcbCommand, IcbDesc, Inherit};
    let Some(ctx) = ctx() else { return };
    let (w, h) = (32u32, 32u32);
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let lib = ShaderLibrary::from_source(&ctx, "draw", DRAW_MSL).unwrap();
    let mut pdesc =
        RenderPipelineDesc::new((&lib, "vs"), Some((&lib, "fs")), &[Format::Rgba8Unorm]);
    pdesc.support_icb = true;
    let pipe = cache.render(41, &pdesc).unwrap();
    let icb = ctx
        .new_indirect_command_buffer(&IcbDesc {
            max_commands: 4,
            inherit: Inherit {
                buffers: true,
                ..Inherit::NOTHING
            },
            max_vertex_buffers: 0,
            max_fragment_buffers: 0,
        })
        .unwrap();
    let draw = IcbCommand::Draw {
        primitive: Primitive::Triangle,
        first_vertex: 0,
        vertex_count: 3,
        instance_count: 1,
        base_instance: 0,
    };
    icb.encode(0, &[IcbCommand::SetPipeline(pipe), draw])
        .unwrap();
    let g = guest(&ctx, MIB);
    let buffer = nxkit_metal::IcbBuffer {
        guest: g,
        offset: 0,
    };
    assert!(
        icb.encode(1, &[IcbCommand::SetVertexBuffer { index: 0, buffer }])
            .is_err()
    );
    assert!(icb.encode(4, &[]).is_err());
    ctx.commit_residency();
    let target = shared_target(&ctx, w, h);
    let table = ArgumentTable::new(&ctx, 1, 1).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    let desc = RenderPassDesc {
        colors: vec![ColorAttachment {
            texture: target.clone(),
            load: LoadAction::Clear,
            store: StoreAction::Store,
            clear: [0.0; 4],
            memoryless: false,
        }],
        ..Default::default()
    };
    let mut pass = frame.commands().begin_render_pass(&desc, &table).unwrap();
    assert!(
        pass.encode(&RenderCommand::ExecuteIcb {
            icb: icb.clone(),
            first: 0,
            count: 5
        })
        .is_err()
    );
    pass.encode(&RenderCommand::ExecuteIcb {
        icb,
        first: 0,
        count: 1,
    })
    .unwrap();
    pass.end();
    frame.submit().unwrap().wait().unwrap();
    assert!(
        pixels(&target, w, h)
            .chunks(4)
            .all(|p| p == [255, 0, 0, 255])
    );
}
