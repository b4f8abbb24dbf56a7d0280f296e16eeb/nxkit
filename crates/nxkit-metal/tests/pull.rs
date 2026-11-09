//! Vertex pulling against the stage_in fetch on a real GPU (`nxkit-gpu` `pull`, `nxb-xlate` `dm_fetch.metal`).
//!
//! A triangle drawn twice, once through a vertex descriptor and once through `dm_fetch` and a `dm_vlayout` block, from
//! the same vertices high in a no-copy guest buffer, must give the same pixels, and neither command buffer may fail.
//! Ignored by default: `cargo test -p nxkit-metal --test pull -- --ignored`.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use nxkit_metal::{
    ArchiveConfig, ArgumentTable, ColorAttachment, Context, Format, FrameRing, GuestMemory,
    LoadAction, PipelineCache, Primitive, RenderCommand, RenderPassDesc, RenderPipelineDesc,
    ShaderLibrary, StepFunction, StoreAction, Texture, TextureDesc, VertexAttribute, VertexFormat,
    VertexLayout,
};

/// The fetch helpers of the `_bp` libraries, as the translator writes them.
const FETCH: &str = include_str!("../../nxb-xlate/src/msl/variant/dm_fetch.metal");

const MIB: u64 = 1024 * 1024;
/// The guest buffer and where the vertices lie in it: near its end, far above the base.
const GUEST: u64 = 256 * MIB;
const VERTS: u64 = GUEST - 64 * 1024;
const SHIFTS: u64 = VERTS + 0x1000;
const BLOCK: u64 = 0x2000;
/// The buffer slot of the layout block and of stream 0 of the stage_in path (`translate::stream_slot(0)`).
const SLOT: u32 = 16;
const W: u32 = 64;

fn shader() -> String {
    format!(
        r#"
#include <metal_stdlib>
using namespace metal;
{FETCH}
struct V {{ float4 pos [[position]]; float4 color; }};
struct In {{ float4 pos [[attribute(0)]]; float4 shift [[attribute(1)]]; float4 color [[attribute(3)]]; }};
vertex V vs_stage(In in [[stage_in]]) {{ return {{ in.pos + in.shift, in.color }}; }}
vertex V vs_pull(constant dm_vlayout& L [[buffer({SLOT})]], uint vid [[vertex_id]], uint iid [[instance_id]],
                 uint bi [[base_instance]]) {{
    float4 pos = as_type<float4>(dm_fetch(L, 0u, vid, iid, bi));
    float4 shift = as_type<float4>(dm_fetch(L, 1u, vid, iid, bi));
    float4 color = as_type<float4>(dm_fetch(L, 3u, vid, iid, bi));
    return {{ pos + shift, color }};
}}
fragment float4 fs(V in [[stage_in]]) {{ return in.color; }}
"#
    )
}

/// Three vertices of 20 bytes (float4 position, RGBA8 colour) and two per-instance float4 shifts.
fn write_vertices(g: &GuestMemory) {
    let verts: [([f32; 4], [u8; 4]); 3] = [
        ([-0.9, -0.9, 0.0, 1.0], [255, 0, 0, 255]),
        ([0.6, -0.8, 0.0, 1.0], [0, 255, 0, 255]),
        ([-0.2, 0.9, 0.0, 1.0], [30, 60, 250, 255]),
    ];
    let mut v = Vec::new();
    for (p, c) in verts {
        p.iter().for_each(|f| v.extend(f.to_le_bytes()));
        v.extend(c);
    }
    g.write(VERTS, &v).unwrap();
    let mut s = Vec::new();
    for f in [0.0f32, 0.0, 0.0, 0.0, 0.25, 0.05, 0.0, 0.0] {
        s.extend(f.to_le_bytes());
    }
    g.write(SHIFTS, &s).unwrap();
}

/// The `dm_vlayout` block: stream 0 per vertex (stride 20), stream 1 per instance (stride 16, divisor 1); input 0
/// float4 (NVN format 0x2E), input 1 float4 from stream 1, input 3 RGBA8 unorm (0x25) at offset 16.
fn write_block(g: &GuestMemory) {
    let mut b = vec![0u8; 320];
    let mut stream = |i: usize, addr: u64, stride: u32, divisor: u32| {
        b[i * 16..i * 16 + 8].copy_from_slice(&addr.to_le_bytes());
        b[i * 16 + 8..i * 16 + 12].copy_from_slice(&stride.to_le_bytes());
        b[i * 16 + 12..i * 16 + 16].copy_from_slice(&divisor.to_le_bytes());
    };
    stream(0, g.gpu_address_of_offset(VERTS, 60).unwrap(), 20, 0);
    stream(1, g.gpu_address_of_offset(SHIFTS, 32).unwrap(), 16, 1);
    for (location, word) in [(0usize, 0x2Eu32), (1, 0x2E | 1 << 27), (3, 0x25 | 16 << 8)] {
        b[256 + 4 * location..260 + 4 * location].copy_from_slice(&word.to_le_bytes());
    }
    g.write(BLOCK, &b).unwrap();
}

fn target(ctx: &Context) -> Texture {
    let mut d = TextureDesc::render_target_2d(W, W, Format::Rgba8Unorm);
    d.storage = nxkit_metal::Storage::Shared;
    Texture::new(ctx, d).unwrap()
}

#[test]
#[ignore = "needs a Metal 4 GPU; run with --ignored"]
fn pulled_vertices_from_high_guest_memory_draw_the_pixels_of_stage_in() {
    let ctx = Context::system_default().expect("a Metal 4 device");
    let g = Arc::new(GuestMemory::allocate(&ctx, GUEST).unwrap());
    write_vertices(&g);
    write_block(&g);
    let lib = ShaderLibrary::from_source(&ctx, "pull-vs-stage-in", &shader()).unwrap();
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let mut stage = RenderPipelineDesc::new(
        (&lib, "vs_stage"),
        Some((&lib, "fs")),
        &[Format::Rgba8Unorm],
    );
    let attr = |format, offset, buffer| VertexAttribute {
        format,
        offset,
        buffer,
    };
    stage.vertex_layout.attributes = vec![
        (0, attr(VertexFormat::Float4, 0, SLOT)),
        (1, attr(VertexFormat::Float4, 0, SLOT + 1)),
        (3, attr(VertexFormat::UChar4Norm, 16, SLOT)),
    ];
    stage.vertex_layout.layouts = vec![
        VertexLayout {
            buffer: SLOT,
            stride: 20,
            step: StepFunction::PerVertex,
            step_rate: 1,
        },
        VertexLayout {
            buffer: SLOT + 1,
            stride: 16,
            step: StepFunction::PerInstance,
            step_rate: 1,
        },
    ];
    let stage = cache.render(1, &stage).unwrap();
    let pull = cache
        .render(
            2,
            &RenderPipelineDesc::new((&lib, "vs_pull"), Some((&lib, "fs")), &[Format::Rgba8Unorm]),
        )
        .unwrap();
    let addr = |off: u64| g.gpu_address_of_offset(off, 1).unwrap();
    let binds_stage = [(SLOT, addr(VERTS)), (SLOT + 1, addr(SHIFTS))];
    let binds_pull = [(SLOT, addr(BLOCK))];
    let (a, b) = (target(&ctx), target(&ctx));
    let table = ArgumentTable::new(&ctx, 31, 0).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    for (t, pipe, binds) in [(&a, &stage, &binds_stage[..]), (&b, &pull, &binds_pull[..])] {
        let desc = RenderPassDesc {
            colors: vec![Some(ColorAttachment {
                depth_plane: 0,
                texture: t.clone(),
                load: LoadAction::Clear,
                store: StoreAction::Store,
                clear: [0.0, 0.0, 0.0, 1.0],
                memoryless: false,
            })],
            depth: None,
            stencil: None,
        };
        let mut pass = frame.commands().begin_render_pass(&desc, &table).unwrap();
        pass.encode(&RenderCommand::SetPipeline(pipe.clone()))
            .unwrap();
        for (index, gpu_addr) in binds {
            pass.encode(&RenderCommand::SetBuffer {
                index: *index,
                gpu_addr: *gpu_addr,
            })
            .unwrap();
        }
        pass.encode(&RenderCommand::Draw {
            primitive: Primitive::Triangle,
            first_vertex: 0,
            vertex_count: 3,
            instance_count: 2,
            base_instance: 0,
        })
        .unwrap();
        pass.end();
    }
    // A GPU address fault fails the command buffer, and `wait` reports it.
    frame.submit().unwrap().wait().expect("no GPU fault");
    let read = |t: &Texture| {
        let mut v = vec![0u8; (W * W * 4) as usize];
        t.read((W * 4) as usize, &mut v).unwrap();
        v
    };
    let (pa, pb) = (read(&a), read(&b));
    let lit = pa.chunks(4).filter(|p| p[..3] != [0, 0, 0]).count();
    assert!(lit > 500, "the triangles cover {lit} pixels");
    let differ = pa
        .chunks(4)
        .zip(pb.chunks(4))
        .filter(|(x, y)| x != y)
        .count();
    assert_eq!(
        differ, 0,
        "pulled and stage_in pixels differ in {differ} places"
    );
}
