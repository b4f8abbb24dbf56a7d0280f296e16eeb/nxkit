//! Tests that need a real Metal 4 GPU. Without a device they print a message and pass.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use nxkit_metal::{
    ArchiveConfig, ArgumentTable, ColorAttachment, ComputeCommand, Context, DeswizzleParams,
    Deswizzler, Format, FrameRing, GobOrder, GuestMemory, LoadAction, MetalError, PipelineCache,
    Presenter, Primitive, RenderCommand, RenderPassDesc, RenderPipelineDesc, ShaderLibrary,
    StoreAction, Texture, TextureDesc, TextureKind, TextureRegion,
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

#[test]
fn scratch_growth_preserves_live_gpu_addresses_and_reuses_chunks() {
    let Some(ctx) = ctx() else { return };
    let scratch = nxkit_metal::Scratch::new(&ctx, 256).unwrap();
    let a = scratch.push(&[1; 256]).unwrap();
    let b = scratch.push(&[2; 512]).unwrap();
    let c = scratch.push(&[3; 256]).unwrap();
    let g = guest(&ctx, MIB);
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let lib = ShaderLibrary::from_source(&ctx, "add", ADD_MSL).unwrap();
    let pipe = cache.compute(10, &lib, "add_words").unwrap();
    let table = ArgumentTable::new(&ctx, 1, 0).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    for (i, src) in [a, b, c].iter().enumerate() {
        let dst = g.gpu_address_of_offset(i as u64 * 256, 256).unwrap();
        let args = frame.scratch().push(&params(*src, dst, 64, 0)).unwrap();
        let mut pass = frame.commands().begin_compute_pass(&table).unwrap();
        pass.encode(&ComputeCommand::SetPipeline(pipe.clone()))
            .unwrap();
        pass.encode(&ComputeCommand::SetBuffer {
            index: 0,
            gpu_addr: args,
        })
        .unwrap();
        pass.encode(&ComputeCommand::DispatchThreads {
            grid: [64, 1, 1],
            threads: [64, 1, 1],
        })
        .unwrap();
        pass.end();
    }
    ctx.commit_residency();
    frame.submit().unwrap().wait().unwrap();
    let mut out = [0; 768];
    g.read(0, &mut out).unwrap();
    assert_eq!(&out[..256], &[1; 256]);
    assert_eq!(&out[256..512], &[2; 256]);
    assert_eq!(&out[512..], &[3; 256]);
    scratch.reset();
    assert_eq!(scratch.push(&[4; 256]).unwrap(), a);
    assert_eq!(scratch.push(&[5; 512]).unwrap(), b);
    assert_eq!(scratch.push(&[6; 256]).unwrap(), c);
}

#[test]
fn constant_vertex_attribute_compiles_with_zero_step_rate() {
    use nxkit_metal::{StepFunction, VertexAttribute, VertexFormat, VertexLayout};
    let Some(ctx) = ctx() else { return };
    let lib = ShaderLibrary::from_source(
        &ctx,
        "constant_attribute",
        r#"
        #include <metal_stdlib>
        using namespace metal;
        struct In { float4 value [[attribute(7)]]; };
        vertex float4 vs(In in [[stage_in]], uint id [[vertex_id]]) {
            return float4(float(id), 0, 0, 1) + in.value;
        }
    "#,
    )
    .unwrap();
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let mut desc = RenderPipelineDesc::new((&lib, "vs"), None, &[]);
    desc.vertex_layout.attributes.push((
        7,
        VertexAttribute {
            format: VertexFormat::Float4,
            offset: 0,
            buffer: 30,
        },
    ));
    desc.vertex_layout.layouts.push(VertexLayout {
        buffer: 30,
        stride: 16,
        step: StepFunction::Constant,
        step_rate: 0,
    });
    cache.render(17, &desc).unwrap();
}

#[test]
fn single_channel_font_view_swizzles_to_white_with_red_as_alpha() {
    use nxkit_metal::{DataBuffer, TextureKind, ViewDesc};
    let Some(ctx) = ctx() else { return };
    let mut desc = TextureDesc::compute_2d(1, 1, Format::R8Unorm);
    desc.allow_views = true;
    let tex = Texture::new(&ctx, desc).unwrap();
    let input = Arc::new(DataBuffer::new(&ctx, 1).unwrap());
    input.write(0, &[128]).unwrap();
    let view = tex
        .view_swizzled(
            &ViewDesc {
                format: Format::R8Unorm,
                kind: TextureKind::D2,
                first_level: 0,
                level_count: 1,
                first_slice: 0,
                slice_count: 1,
            },
            [1, 1, 1, 2],
        )
        .unwrap();
    let output = Arc::new(DataBuffer::new(&ctx, 16).unwrap());
    let lib = ShaderLibrary::from_source(&ctx, "font_swizzle", r#"
        #include <metal_stdlib>
        using namespace metal;
        kernel void sample_font(device float4* out [[buffer(0)]], texture2d<float> tex [[texture(0)]]) {
            constexpr sampler s(coord::normalized, filter::nearest);
            out[0] = tex.sample(s, float2(0.5));
        }
    "#).unwrap();
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let pipeline = cache.compute(99, &lib, "sample_font").unwrap();
    let table = ArgumentTable::new(&ctx, 1, 1).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    {
        let mut cp = frame.commands().begin_compute_pass(&table).unwrap();
        cp.encode(&ComputeCommand::CopyBufferToTexture {
            buffer: input,
            offset: 0,
            bytes_per_row: 1,
            texture: tex,
            region: nxkit_metal::TextureRegion::whole(0, 0, [1; 3]),
        })
        .unwrap();
        cp.encode(&ComputeCommand::Depend(
            nxkit_metal::Dependency::within_pass(
                nxkit_metal::Use::CopyWrite,
                nxkit_metal::Use::ComputeRead,
            ),
        ))
        .unwrap();
        cp.encode(&ComputeCommand::SetPipeline(pipeline)).unwrap();
        cp.encode(&ComputeCommand::SetBuffer {
            index: 0,
            gpu_addr: output.gpu_address(),
        })
        .unwrap();
        cp.encode(&ComputeCommand::SetTexture {
            index: 0,
            texture: view,
        })
        .unwrap();
        cp.encode(&ComputeCommand::DispatchThreads {
            grid: [1; 3],
            threads: [1; 3],
        })
        .unwrap();
        cp.retain(output.clone());
        cp.end();
    }
    ctx.commit_residency();
    frame.submit().unwrap().wait().unwrap();
    let mut bytes = [0; 16];
    output.read(0, &mut bytes).unwrap();
    let rgba: Vec<f32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    assert_eq!(&rgba[..3], &[1.0; 3]);
    assert!((rgba[3] - 128.0 / 255.0).abs() < 0.0001);
}

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
        colors: vec![Some(ColorAttachment {
            depth_plane: 0,
            texture: t.clone(),
            load: LoadAction::Clear,
            store: StoreAction::Store,
            clear: [0.0, 0.0, 1.0, 1.0],
            memoryless: false,
        })],
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
fn a_box_goes_from_guest_memory_into_a_texture_and_back() {
    let Some(ctx) = ctx() else { return };
    let (w, h) = (16u32, 8u32);
    let g = guest(&ctx, 2 * MIB);
    let tex = shared_target(&ctx, w, h);
    // Rows of 4 pixels at a pitch of 32 bytes, so the box does not read packed bytes.
    let src = pattern(32 * 3, 7);
    g.write(4096, &src).unwrap();
    let region = TextureRegion {
        level: 0,
        slice: 0,
        origin: [3, 2, 0],
        size: [4, 3, 1],
    };
    let table = ArgumentTable::new(&ctx, 1, 1).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    let mut pass = frame.commands().begin_compute_pass(&table).unwrap();
    let up =
        |offset, bytes_per_row, texture: &Texture, region| ComputeCommand::CopyGuestToTexture {
            guest: g.clone(),
            offset,
            bytes_per_row,
            bytes_per_image: 0,
            texture: texture.clone(),
            region,
        };
    pass.encode(&up(4096, 32, &tex, region)).unwrap();
    // A pitch below one row of blocks and a box outside the level are refused.
    assert!(pass.encode(&up(4096, 8, &tex, region)).is_err());
    assert!(
        pass.encode(&up(
            4096,
            32,
            &tex,
            TextureRegion {
                origin: [14, 0, 0],
                ..region
            }
        ))
        .is_err()
    );
    assert!(pass.encode(&up(2 * MIB - 8, 32, &tex, region)).is_err());
    pass.encode(&ComputeCommand::Barrier).unwrap();
    pass.encode(&ComputeCommand::CopyTextureRegionToGuest {
        texture: tex.clone(),
        region,
        guest: g.clone(),
        offset: 8192,
        bytes_per_row: 16,
        bytes_per_image: 0,
    })
    .unwrap();
    pass.end();
    frame.submit().unwrap().wait().unwrap();
    let all = pixels(&tex, w, h);
    let mut back = vec![0u8; 16 * 3];
    g.read(8192, &mut back).unwrap();
    for row in 0..3usize {
        let at = ((2 + row) * w as usize + 3) * 4;
        assert_eq!(&all[at..at + 16], &src[row * 32..row * 32 + 16]);
        assert_eq!(
            &back[row * 16..row * 16 + 16],
            &src[row * 32..row * 32 + 16]
        );
    }
}

#[test]
fn scratch_bytes_fill_a_box_of_a_texture() {
    let Some(ctx) = ctx() else { return };
    let (w, h) = (16u32, 8u32);
    let tex = shared_target(&ctx, w, h);
    let table = ArgumentTable::new(&ctx, 1, 1).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    let scratch = frame.scratch();
    // Two rows of 5 pixels in one colour.
    let bytes: Vec<u8> = [10u8, 20, 30, 40].repeat(10);
    let address = scratch.push(&bytes).unwrap();
    let region = TextureRegion {
        level: 0,
        slice: 0,
        origin: [2, 3, 0],
        size: [5, 2, 1],
    };
    let mut pass = frame.commands().begin_compute_pass(&table).unwrap();
    pass.copy_scratch_to_texture(&scratch, address, 20, &tex, &region)
        .unwrap();
    // A range that is not in the scratch, a box that is too deep and a box outside the texture are refused.
    assert!(
        pass.copy_scratch_to_texture(&scratch, 16, 20, &tex, &region)
            .is_err()
    );
    assert!(
        pass.copy_scratch_to_texture(
            &scratch,
            address,
            20,
            &tex,
            &TextureRegion {
                size: [5, 2, 2],
                ..region
            }
        )
        .is_err()
    );
    assert!(
        pass.copy_scratch_to_texture(
            &scratch,
            address,
            20,
            &tex,
            &TextureRegion {
                origin: [14, 0, 0],
                ..region
            }
        )
        .is_err()
    );
    pass.end();
    frame.submit().unwrap().wait().unwrap();
    let all = pixels(&tex, w, h);
    for y in 0..h as usize {
        for x in 0..w as usize {
            let inside = (2..7).contains(&x) && (3..5).contains(&y);
            let at = (y * w as usize + x) * 4;
            let want = if inside { [10, 20, 30, 40] } else { [0; 4] };
            assert_eq!(all[at..at + 4], want, "pixel {x},{y}");
        }
    }
}

#[test]
fn all_render_targets_keep_their_shader_output_slots() {
    let Some(ctx) = ctx() else { return };
    let source = format!(
        "{DRAW_MSL}\n{}",
        r#"
struct Outputs { float4 a [[color(0)]]; float4 b [[color(1)]];
    float4 c [[color(2)]]; float4 d [[color(3)]]; };
fragment Outputs mrt() { return {float4(1,0,0,1), float4(0,1,0,1),
    float4(0,0,1,1), float4(1)}; }
"#
    );
    let lib = ShaderLibrary::from_source(&ctx, "mrt", &source).unwrap();
    let formats = [
        Format::Rgba8Unorm,
        Format::Rgba8UnormSrgb,
        Format::Rgba8Unorm,
        Format::Rgba8Unorm,
    ];
    let targets: Vec<_> = formats
        .iter()
        .map(|format| {
            let mut desc = TextureDesc::render_target_2d(16, 16, *format);
            desc.storage = nxkit_metal::Storage::Shared;
            Texture::new(&ctx, desc).unwrap()
        })
        .collect();
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let pipeline = cache
        .render(
            21,
            &RenderPipelineDesc::new((&lib, "vs"), Some((&lib, "mrt")), &formats),
        )
        .unwrap();
    let table = ArgumentTable::new(&ctx, 1, 1).unwrap();
    let desc = RenderPassDesc {
        colors: targets
            .iter()
            .map(|texture| {
                Some(ColorAttachment {
                    depth_plane: 0,
                    texture: texture.clone(),
                    load: LoadAction::Clear,
                    store: StoreAction::Store,
                    clear: [0.0; 4],
                    memoryless: false,
                })
            })
            .collect(),
        depth: None,
        stencil: None,
    };
    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    let mut pass = frame.commands().begin_render_pass(&desc, &table).unwrap();
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
    let expected = [
        [255, 0, 0, 255],
        [0, 255, 0, 255],
        [0, 0, 255, 255],
        [255; 4],
    ];
    for (slot, (texture, color)) in targets.iter().zip(expected).enumerate() {
        assert!(
            pixels(texture, 16, 16)
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| *pixel == color),
            "output slot {slot}"
        );
    }
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
        colors: vec![Some(ColorAttachment {
            depth_plane: 0,
            texture: t,
            load: LoadAction::Clear,
            store: StoreAction::Store,
            clear: [0.0; 4],
            memoryless: true,
        })],
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
            colors: vec![Some(ColorAttachment {
                depth_plane: 0,
                texture: drawable.texture().clone(),
                load: LoadAction::Clear,
                store: StoreAction::Store,
                clear: [0.0, 1.0, 0.0, 1.0],
                memoryless: false,
            })],
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
    desc.colors = vec![Some(
        ColorTarget::new(Format::Rgba8Unorm).with_blend(BlendState::ALPHA),
    )];
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
        vec![Some(ColorAttachment {
            depth_plane: 0,
            texture: t.clone(),
            load: LoadAction::Clear,
            store: StoreAction::Store,
            clear: [0.0, 0.0, 1.0, 1.0],
            memoryless: false,
        })]
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
        colors: vec![Some(ColorAttachment {
            depth_plane: 0,
            texture: target.clone(),
            load: LoadAction::Clear,
            store: StoreAction::Store,
            clear: [0.0; 4],
            memoryless: false,
        })],
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
        colors: vec![Some(ColorAttachment {
            depth_plane: 0,
            texture: target.clone(),
            load: LoadAction::Clear,
            store: StoreAction::Store,
            clear: [0.0; 4],
            memoryless: false,
        })],
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

#[test]
fn render_targets_select_distinct_depth_planes_of_a_volume() {
    let Some(ctx) = ctx() else { return };
    let mut desc = TextureDesc::render_target_2d(16, 16, Format::Rgba8Unorm);
    desc.kind = TextureKind::D3;
    desc.depth = 2;
    let volume = Texture::new(&ctx, desc).unwrap();
    let outputs = [shared_target(&ctx, 16, 16), shared_target(&ctx, 16, 16)];
    let table = ArgumentTable::new(&ctx, 1, 1).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    for (plane, clear) in [[1., 0., 0., 1.], [0., 1., 0., 1.]].into_iter().enumerate() {
        frame
            .commands()
            .begin_render_pass(
                &RenderPassDesc {
                    colors: vec![Some(ColorAttachment {
                        texture: volume.clone(),
                        depth_plane: plane as u32,
                        load: LoadAction::Clear,
                        store: StoreAction::Store,
                        clear,
                        memoryless: false,
                    })],
                    depth: None,
                    stencil: None,
                },
                &table,
            )
            .unwrap()
            .end();
    }
    let mut pass = frame.commands().begin_compute_pass(&table).unwrap();
    pass.encode(&ComputeCommand::WaitForEarlierPasses).unwrap();
    for (plane, dst) in outputs.iter().enumerate() {
        pass.encode(&ComputeCommand::CopyTexture {
            src: volume.clone(),
            dst: dst.clone(),
            src_region: TextureRegion {
                level: 0,
                slice: 0,
                origin: [0, 0, plane as u32],
                size: [16, 16, 1],
            },
            dst_level: 0,
            dst_slice: 0,
            dst_origin: [0; 3],
        })
        .unwrap();
    }
    pass.end();
    frame.submit().unwrap().wait().unwrap();
    for (texture, expected) in outputs.iter().zip([[255, 0, 0, 255], [0, 255, 0, 255]]) {
        assert!(
            pixels(texture, 16, 16)
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| *pixel == expected)
        );
    }
}

#[test]
fn an_uncompressed_view_of_a_compressed_texture_is_an_error() {
    let Some(ctx) = ctx() else { return };
    let mut desc = TextureDesc::compute_2d(16, 16, Format::Bc(nxkit_metal::BcKind::Bc4Unorm));
    desc.shader_write = false;
    desc.storage = nxkit_metal::Storage::Private;
    desc.allow_views = true;
    let tex = Texture::new(&ctx, desc).unwrap();
    let view = tex.view(&nxkit_metal::ViewDesc {
        format: Format::Rgba16Uint,
        kind: TextureKind::D2,
        first_level: 0,
        level_count: 1,
        first_slice: 0,
        slice_count: 1,
    });
    assert!(matches!(view, Err(MetalError::Invalid(_))));
}

const MESH_MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;
struct V { float4 position [[position]]; };
using M = metal::mesh<V, void, 3, 1, metal::topology::triangle>;
[[mesh]] void ms(M out, uint t [[thread_index_in_threadgroup]]) {
    if (t == 0) {
        out.set_primitive_count(1);
        out.set_vertex(0, V{float4(-1, -1, 0, 1)});
        out.set_vertex(1, V{float4(3, -1, 0, 1)});
        out.set_vertex(2, V{float4(-1, 3, 0, 1)});
        out.set_index(0, 0);
        out.set_index(1, 1);
        out.set_index(2, 2);
    }
}
fragment float4 green() { return float4(0, 1, 0, 1); }
vertex V full(uint id [[vertex_id]]) {
    float2 p = float2((id << 1) & 2, id & 2);
    return {float4(p * 2 - 1, 0, 1)};
}
"#;

#[test]
fn a_mesh_pipeline_draws_a_grid_and_a_depth_only_one_builds() {
    let Some(ctx) = ctx() else { return };
    let lib = ShaderLibrary::from_source(&ctx, "mesh", MESH_MSL).unwrap();
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let desc = |fragment| nxkit_metal::MeshPipelineDesc {
        object: None,
        mesh: (&lib, "ms"),
        fragment,
        colors: vec![Some(nxkit_metal::ColorTarget::new(Format::Rgba8Unorm))],
        sample_count: 1,
        alpha_to_coverage: false,
        object_threads: 0,
        mesh_threads: 1,
        payload_bytes: 0,
    };
    let pipeline = cache.mesh(31, &desc(Some((&lib, "green")))).unwrap();
    cache.mesh(32, &desc(None)).unwrap();
    let target = shared_target(&ctx, 16, 16);
    let table = ArgumentTable::new(&ctx, 1, 1).unwrap();
    let pass_desc = RenderPassDesc {
        colors: vec![Some(ColorAttachment {
            depth_plane: 0,
            texture: target.clone(),
            load: LoadAction::Clear,
            store: StoreAction::Store,
            clear: [0.0; 4],
            memoryless: false,
        })],
        depth: None,
        stencil: None,
    };
    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    let mut pass = frame
        .commands()
        .begin_render_pass(&pass_desc, &table)
        .unwrap();
    pass.encode(&RenderCommand::SetPipeline(pipeline)).unwrap();
    pass.encode(&RenderCommand::DrawMeshThreadgroups {
        groups: [1, 1, 1],
        object_threads: 1,
        mesh_threads: 1,
    })
    .unwrap();
    pass.end();
    frame.submit().unwrap().wait().unwrap();
    assert!(
        pixels(&target, 16, 16)
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| *p == [0, 255, 0, 255])
    );
}

#[test]
fn an_indirect_draw_reads_its_arguments_on_the_gpu() {
    let Some(ctx) = ctx() else { return };
    let lib = ShaderLibrary::from_source(&ctx, "mesh", MESH_MSL).unwrap();
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let pipeline = cache
        .render(
            33,
            &RenderPipelineDesc::new((&lib, "full"), Some((&lib, "green")), &[Format::Rgba8Unorm]),
        )
        .unwrap();
    let args = nxkit_metal::DataBuffer::new(&ctx, 256).unwrap();
    // vertex count, instance count, first vertex, first instance
    let words: Vec<u8> = [3u32, 1, 0, 0]
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect();
    args.write(0, &words).unwrap();
    let target = shared_target(&ctx, 16, 16);
    let table = ArgumentTable::new(&ctx, 1, 1).unwrap();
    let pass_desc = RenderPassDesc {
        colors: vec![Some(ColorAttachment {
            depth_plane: 0,
            texture: target.clone(),
            load: LoadAction::Clear,
            store: StoreAction::Store,
            clear: [0.0; 4],
            memoryless: false,
        })],
        depth: None,
        stencil: None,
    };
    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    let mut pass = frame
        .commands()
        .begin_render_pass(&pass_desc, &table)
        .unwrap();
    pass.encode(&RenderCommand::SetPipeline(pipeline)).unwrap();
    assert!(
        pass.encode(&RenderCommand::DrawIndirect {
            primitive: Primitive::Triangle,
            indirect_addr: args.gpu_address() + 2
        })
        .is_err()
    );
    pass.encode(&RenderCommand::DrawIndirect {
        primitive: Primitive::Triangle,
        indirect_addr: args.gpu_address(),
    })
    .unwrap();
    pass.end();
    frame.submit().unwrap().wait().unwrap();
    assert!(
        pixels(&target, 16, 16)
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| *p == [0, 255, 0, 255])
    );
}

/// A pipeline found in the archive is told from one that was compiled, and a miss is explained.
#[test]
fn archive_hits_and_misses_are_told_apart() {
    use nxkit_metal::Source;
    let Some(ctx) = ctx() else { return };
    let dir = std::env::temp_dir().join(format!("nxkit-metal-trace-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let archive = dir.join("p.mtl4archive");
    let lib = ShaderLibrary::from_source(&ctx, "draw", DRAW_MSL).unwrap();
    let desc = || RenderPipelineDesc::new((&lib, "vs"), Some((&lib, "fs")), &[Format::Rgba8Unorm]);
    let first = PipelineCache::new(
        &ctx,
        &ArchiveConfig {
            collect: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(matches!(
        first.render_traced(1, &desc()).unwrap().1,
        Source::Compiled(_)
    ));
    assert_eq!(first.render_traced(1, &desc()).unwrap().1, Source::Cached);
    first.save_archive(&archive).unwrap();
    let second = PipelineCache::new(
        &ctx,
        &ArchiveConfig {
            load: Some(archive),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(second.render_traced(1, &desc()).unwrap().1, Source::Archive);
    let mut other = desc();
    other.fragment = None;
    let why = second.explain_miss(&other).unwrap();
    eprintln!("{why:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn image_atomics_get_an_r32_view_over_a_four_byte_colour_texture() {
    use nxkit_metal::ViewDesc;
    let Some(ctx) = ctx() else { return };
    let base = |format| TextureDesc {
        kind: TextureKind::D2,
        width: 16,
        height: 16,
        depth: 1,
        mip_levels: 1,
        array_layers: 1,
        allow_views: true,
        format,
        shader_read: true,
        shader_write: true,
        render_target: true,
        storage: nxkit_metal::Storage::Private,
        shader_atomic: true,
    };
    let view = |format| ViewDesc {
        format,
        kind: TextureKind::D2,
        first_level: 0,
        level_count: 1,
        first_slice: 0,
        slice_count: 1,
    };
    // RGBA8 is stored as R32Uint: it sees itself as RGBA8 and the atomics see R32Uint and R32Sint
    let rgba = Texture::new(&ctx, base(Format::Rgba8Unorm)).unwrap();
    assert_eq!(rgba.desc().format, Format::Rgba8Unorm);
    assert_eq!(
        rgba.atomic_view(&view(Format::Rgba8Unorm), false)
            .unwrap()
            .desc()
            .format,
        Format::R32Uint
    );
    assert_eq!(
        rgba.atomic_view(&view(Format::Rgba8Unorm), true)
            .unwrap()
            .desc()
            .format,
        Format::R32Sint
    );
    // an R32Uint texture serves itself
    let r32 = Texture::new(&ctx, base(Format::R32Uint)).unwrap();
    assert_eq!(
        r32.atomic_view(&view(Format::R32Uint), false)
            .unwrap()
            .desc()
            .format,
        Format::R32Uint
    );
    // a texture made without the flag cannot
    let plain = Texture::new(
        &ctx,
        TextureDesc {
            shader_atomic: false,
            ..base(Format::Rgba8Unorm)
        },
    )
    .unwrap();
    assert!(plain.atomic_view(&view(Format::Rgba8Unorm), false).is_err());
}
