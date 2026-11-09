//! A draw that is gated by a visibility count, decided on the GPU inside one command buffer.
//!
//! A game can draw a small test box, let the GPU count the samples that pass the depth test, and then draw
//! something only if the count is not zero. The runtime needs the same inside one frame, without a stop for the
//! CPU. These tests prove that Metal allows it. In one command buffer:
//!
//! 1. A render pass counts the samples of two query triangles into a visibility result buffer. One passes the depth
//!    test and one is rejected by it.
//! 2. A compute pass reads the two 64-bit counts and writes the gate: the arguments of an indirect draw, or the
//!    range of an indirect compute command buffer. A count of zero gives an empty draw or an empty range.
//! 3. A second render pass (or a second compute pass) runs the gated work. Only the visible query shows a result.
//!
//! The two barriers (pass to pass, with `Device` visibility) are part of what is proven. The CPU reads the counts
//! after the GPU is done, so a failure tells which part of the chain broke.
//!
//! Needs a Metal 4 GPU. Without one the tests print a message and pass.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use nxkit_metal::{
    ArchiveConfig, ArgumentTable, ColorAttachment, CommandBuffer, CompareFn, ComputeCommand,
    ComputeIcbCommand, ComputeIcbDesc, Context, DataBuffer, Dependency, DepthAttachment,
    DepthStencilDesc, DepthStencilState, Format, FrameRing, LoadAction, PipelineCache, Primitive,
    RenderCommand, RenderPassDesc, RenderPipelineDesc, ShaderLibrary, Stages, Storage, StoreAction,
    Texture, TextureDesc, Use, Visibility,
};

const MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;

// Pass A: a full-screen triangle. The test only needs the count, not the colour.
struct V { float4 pos [[position]]; };
vertex V vs_query(uint id [[vertex_id]]) {
    float2 p = float2((id << 1) & 2, id & 2);
    V v; v.pos = float4(p * 2.0 - 1.0, 0.5, 1.0); return v;
}
fragment float4 fs_query(V in [[stage_in]]) { return float4(1.0); }

// Pass B: a quad of six vertices on the left half (instance 0) or the right half (instance 1).
// The first instance of the indirect arguments is the query number.
struct H { float4 pos [[position]]; float4 col; };
vertex H vs_half(uint vid [[vertex_id]], uint inst [[instance_id]]) {
    float x0 = inst == 0 ? -1.0 : 0.0;
    float fx = (vid == 1 || vid == 4 || vid == 5) ? 1.0 : 0.0;
    float fy = (vid == 2 || vid == 3 || vid == 5) ? 1.0 : 0.0;
    H h;
    h.pos = float4(x0 + fx, -1.0 + 2.0 * fy, 0.0, 1.0);
    h.col = inst == 0 ? float4(0.0, 1.0, 0.0, 1.0) : float4(0.0, 0.0, 1.0, 1.0);
    return h;
}
fragment float4 fs_half(H in [[stage_in]]) { return in.col; }

// The gate. One thread per query. It writes the arguments of a DrawPrimitives indirect draw:
// vertex count, instance count, first vertex, first instance.
struct GateParams { ulong vis; ulong out; };
kernel void gate_draw(constant GateParams& p [[buffer(0)]], uint q [[thread_position_in_grid]]) {
    device const ulong* vis = (device const ulong*)p.vis;
    device uint* a = ((device uint*)p.out) + q * 4;
    a[0] = vis[q] != 0 ? 6 : 0;
    a[1] = 1;
    a[2] = 0;
    a[3] = q;
}

// The gate for indirect command buffers: one execution range (location, length) per query.
kernel void gate_range(constant GateParams& p [[buffer(0)]], uint q [[thread_position_in_grid]]) {
    device const ulong* vis = (device const ulong*)p.vis;
    device uint* r = ((device uint*)p.out) + q * 2;
    r[0] = 0;
    r[1] = vis[q] != 0 ? 1 : 0;
}

// The gated work: it leaves a marker.
constant uint MARKER = 0xC0FFEE;
kernel void mark(device uint* m [[buffer(0)]]) { m[0] = MARKER; }
"#;

const MARKER: u32 = 0x00C0_FFEE;

fn ctx() -> Option<Context> {
    match Context::system_default() {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!("skipped: no usable Metal 4 device ({e})");
            None
        }
    }
}

fn u64_at(b: &DataBuffer, offset: u64) -> u64 {
    let mut v = [0u8; 8];
    b.read(offset, &mut v).unwrap();
    u64::from_le_bytes(v)
}

fn u32_at(b: &DataBuffer, offset: u64) -> u32 {
    let mut v = [0u8; 4];
    b.read(offset, &mut v).unwrap();
    u32::from_le_bytes(v)
}

fn shared_target(ctx: &Context, w: u32, h: u32) -> Texture {
    let mut d = TextureDesc::render_target_2d(w, h, Format::Rgba8Unorm);
    d.storage = Storage::Shared;
    Texture::new(ctx, d).unwrap()
}

/// The parameters of a gate kernel: the address of the visibility buffer and the address of its output.
fn gate_params(ctx: &Context, vis: &DataBuffer, out: &DataBuffer) -> DataBuffer {
    let params = DataBuffer::new(ctx, 16).unwrap();
    params.write(0, &vis.gpu_address().to_le_bytes()).unwrap();
    params.write(8, &out.gpu_address().to_le_bytes()).unwrap();
    params
}

/// Pass A. Query 0 (offset 0) is drawn with a depth test that passes, query 1 (offset 8) with one that never
/// passes. Both triangles cover the whole target.
fn encode_queries(
    ctx: &Context,
    cache: &PipelineCache,
    lib: &ShaderLibrary,
    cb: &mut CommandBuffer,
    vis: &DataBuffer,
) {
    let pipeline = cache
        .render(
            1,
            &RenderPipelineDesc::new(
                (lib, "vs_query"),
                Some((lib, "fs_query")),
                &[Format::Rgba8Unorm],
            ),
        )
        .unwrap();
    let state = |compare| {
        DepthStencilState::new(
            ctx,
            &DepthStencilDesc {
                depth_compare: compare,
                depth_write: false,
                ..DepthStencilDesc::OFF
            },
        )
        .unwrap()
    };
    let depth = Texture::new(
        ctx,
        TextureDesc::render_target_2d(16, 16, Format::Depth32Float),
    )
    .unwrap();
    let desc = RenderPassDesc {
        colors: vec![Some(ColorAttachment {
            depth_plane: 0,
            texture: shared_target(ctx, 16, 16),
            load: LoadAction::Clear,
            store: StoreAction::DontCare,
            clear: [0.0; 4],
            memoryless: false,
        })],
        depth: Some(DepthAttachment {
            texture: depth,
            load: LoadAction::Clear,
            store: StoreAction::DontCare,
            clear: 1.0,
            memoryless: false,
        }),
        stencil: None,
    };
    let (vt, ft) = (
        ArgumentTable::new(ctx, 1, 1).unwrap(),
        ArgumentTable::new(ctx, 1, 1).unwrap(),
    );
    let mut pass = cb
        .begin_render_pass_counted(&desc, &vt, &ft, Some(vis))
        .unwrap();
    pass.encode(&RenderCommand::SetPipeline(pipeline)).unwrap();
    for (offset, compare) in [(0, CompareFn::Always), (8, CompareFn::Never)] {
        pass.encode(&RenderCommand::SetDepthStencil(state(compare)))
            .unwrap();
        pass.encode(&RenderCommand::SetVisibility {
            counting: true,
            offset,
        })
        .unwrap();
        pass.encode(&RenderCommand::Draw {
            primitive: Primitive::Triangle,
            first_vertex: 0,
            vertex_count: 3,
            instance_count: 1,
            base_instance: 0,
        })
        .unwrap();
    }
    pass.end();
}

/// The barrier between pass A and a compute pass that reads the counts. Pass A writes the visibility buffer in the
/// fragment stage. The stages on the writing side are all of them, so the test does not depend on which stage
/// Metal uses for the count.
fn after_queries() -> Dependency {
    Dependency {
        after: Stages::ALL,
        ..Dependency::between(Use::AttachmentWrite, Use::ComputeRead)
    }
}

#[test]
fn a_count_of_zero_turns_an_indirect_draw_into_an_empty_one() {
    let Some(ctx) = ctx() else { return };
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let lib = ShaderLibrary::from_source(&ctx, "gate", MSL).unwrap();
    let gate = cache.compute(2, &lib, "gate_draw").unwrap();
    let draw = cache
        .render(
            3,
            &RenderPipelineDesc::new(
                (&lib, "vs_half"),
                Some((&lib, "fs_half")),
                &[Format::Rgba8Unorm],
            ),
        )
        .unwrap();
    let (w, h) = (32u32, 16u32);
    let target = shared_target(&ctx, w, h);
    let vis = DataBuffer::new(&ctx, 16).unwrap();
    let args = DataBuffer::new(&ctx, 32).unwrap();
    let params = gate_params(&ctx, &vis, &args);
    let table = ArgumentTable::new(&ctx, 1, 0).unwrap();
    let draw_table = ArgumentTable::new(&ctx, 1, 1).unwrap();

    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    let cb = frame.commands();

    // 1. The queries.
    encode_queries(&ctx, &cache, &lib, cb, &vis);

    // 2. The gate: one thread per query writes the arguments of its indirect draw.
    {
        let mut pass = cb.begin_compute_pass(&table).unwrap();
        pass.encode(&ComputeCommand::Depend(after_queries()))
            .unwrap();
        pass.encode(&ComputeCommand::SetPipeline(gate)).unwrap();
        pass.encode(&ComputeCommand::SetBuffer {
            index: 0,
            gpu_addr: params.gpu_address(),
        })
        .unwrap();
        pass.encode(&ComputeCommand::DispatchThreads {
            grid: [2, 1, 1],
            threads: [2, 1, 1],
        })
        .unwrap();
        pass.end();
    }

    // 3. The gated draws. The arguments are read when the draw runs, so the pass waits for the compute pass first.
    {
        let desc = RenderPassDesc {
            colors: vec![Some(ColorAttachment {
                depth_plane: 0,
                texture: target.clone(),
                load: LoadAction::Clear,
                store: StoreAction::Store,
                clear: [0.0, 0.0, 0.0, 1.0],
                memoryless: false,
            })],
            depth: None,
            stencil: None,
        };
        let mut pass = cb.begin_render_pass(&desc, &draw_table).unwrap();
        pass.encode(&RenderCommand::Depend(Dependency::between(
            Use::ComputeWrite,
            Use::VertexRead,
        )))
        .unwrap();
        pass.encode(&RenderCommand::SetPipeline(draw)).unwrap();
        for q in 0..2u64 {
            pass.encode(&RenderCommand::DrawIndirect {
                primitive: Primitive::Triangle,
                indirect_addr: args.gpu_address() + q * 16,
            })
            .unwrap();
        }
        pass.end();
    }
    ctx.commit_residency();
    frame.submit().unwrap().wait().unwrap();

    // The first half of the chain: the counts.
    let (c0, c1) = (u64_at(&vis, 0), u64_at(&vis, 8));
    assert!(c0 > 0, "the visible query counted no sample: {c0}");
    assert_eq!(c1, 0, "the hidden query counted samples");
    // The middle: the arguments.
    assert_eq!(
        u32_at(&args, 0),
        6,
        "the gate did not keep the vertex count of the visible query"
    );
    assert_eq!(
        u32_at(&args, 16),
        0,
        "the gate did not empty the draw of the hidden query"
    );
    // The end: the pixels. Left half green for query 0, right half still black for query 1.
    let mut px = vec![0u8; (w * h * 4) as usize];
    target.read((w * 4) as usize, &mut px).unwrap();
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let want = if x < w / 2 {
                [0, 255, 0, 255]
            } else {
                [0, 0, 0, 255]
            };
            assert_eq!(&px[i..i + 4], &want, "pixel ({x}, {y})");
        }
    }
}

/// There is no indirect dispatch with a thread-group count in a buffer in the crate (`ComputeCommand` has none).
/// The closest is a compute indirect command buffer with one dispatch, run through an execution range that the
/// GPU reads from a buffer. The gate writes that range: length 1 for the visible query, 0 for the hidden one. The
/// result is the same: the kernel runs zero times.
#[test]
fn a_count_of_zero_turns_an_indirect_compute_execution_into_an_empty_one() {
    let Some(ctx) = ctx() else { return };
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let lib = ShaderLibrary::from_source(&ctx, "gate", MSL).unwrap();
    let gate = cache.compute(2, &lib, "gate_range").unwrap();
    let mark = cache.compute_icb(3, &lib, "mark").unwrap();
    let vis = DataBuffer::new(&ctx, 16).unwrap();
    // Two ranges of 8 bytes: location, length.
    let ranges = Arc::new(DataBuffer::new(&ctx, 16).unwrap());
    let params = gate_params(&ctx, &vis, &ranges);
    let markers = [
        Arc::new(DataBuffer::new(&ctx, 16).unwrap()),
        Arc::new(DataBuffer::new(&ctx, 16).unwrap()),
    ];
    let icbs: Vec<_> = markers
        .iter()
        .map(|m| {
            let icb = ctx
                .new_compute_indirect_command_buffer(&ComputeIcbDesc {
                    max_commands: 1,
                    max_kernel_buffers: 1,
                })
                .unwrap();
            icb.encode(
                0,
                &[
                    ComputeIcbCommand::SetPipeline(mark.clone()),
                    ComputeIcbCommand::SetKernelData {
                        index: 0,
                        buffer: m.clone(),
                        offset: 0,
                    },
                    ComputeIcbCommand::DispatchThreadgroups {
                        groups: [1, 1, 1],
                        threads: [1, 1, 1],
                    },
                ],
            )
            .unwrap();
            icb
        })
        .collect();
    let table = ArgumentTable::new(&ctx, 1, 0).unwrap();

    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    let cb = frame.commands();
    encode_queries(&ctx, &cache, &lib, cb, &vis);
    {
        let mut pass = cb.begin_compute_pass(&table).unwrap();
        pass.encode(&ComputeCommand::Depend(after_queries()))
            .unwrap();
        pass.encode(&ComputeCommand::SetPipeline(gate)).unwrap();
        pass.encode(&ComputeCommand::SetBuffer {
            index: 0,
            gpu_addr: params.gpu_address(),
        })
        .unwrap();
        pass.encode(&ComputeCommand::DispatchThreads {
            grid: [2, 1, 1],
            threads: [2, 1, 1],
        })
        .unwrap();
        pass.end();
    }
    {
        let mut pass = cb.begin_compute_pass(&table).unwrap();
        pass.encode(&ComputeCommand::Depend(Dependency {
            visibility: Visibility::Device,
            ..Dependency::between(Use::ComputeWrite, Use::ComputeRead)
        }))
        .unwrap();
        for (q, icb) in icbs.iter().enumerate() {
            pass.encode(&ComputeCommand::ExecuteIcbIndirect {
                icb: icb.clone(),
                range: ranges.clone(),
                range_offset: q as u64 * 8,
            })
            .unwrap();
        }
        pass.end();
    }
    ctx.commit_residency();
    frame.submit().unwrap().wait().unwrap();

    let (c0, c1) = (u64_at(&vis, 0), u64_at(&vis, 8));
    assert!(c0 > 0, "the visible query counted no sample: {c0}");
    assert_eq!(c1, 0, "the hidden query counted samples");
    assert_eq!(
        u32_at(&ranges, 4),
        1,
        "the gate did not open the range of the visible query"
    );
    assert_eq!(
        u32_at(&ranges, 12),
        0,
        "the gate did not close the range of the hidden query"
    );
    assert_eq!(
        u32_at(&markers[0], 0),
        MARKER,
        "the visible query did not run its kernel"
    );
    assert_eq!(u32_at(&markers[1], 0), 0, "the hidden query ran its kernel");
}
