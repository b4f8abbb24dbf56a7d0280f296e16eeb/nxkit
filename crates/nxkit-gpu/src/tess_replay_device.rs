//! Targeted Metal 4 smoke test using the translator-generated synthetic shader set.
//! The ordinary unit suite does not execute it or require the fixture files.

use super::*;
use nxkit_metal::{
    ArchiveConfig, ArgumentTable, ColorAttachment, ComputeCommand, Context, Format, FrameRing,
    LoadAction, PipelineCache, RenderPassDesc, RenderPipelineDesc, Scratch, ShaderLibrary, Storage,
    StoreAction, Texture, TextureDesc, TopologyClass,
};

#[test]
#[ignore = "requires Apple hardware and explicitly staged synthetic MSL fixture"]
fn gpu_written_indices_replay_in_order_and_invalidate_only_the_bad_patch() {
    let dir = std::path::PathBuf::from(std::env::var_os("NXKIT_TESS_FIXTURE_DIR").unwrap());
    let ctx = Context::system_default().unwrap();
    let lib = |name| {
        let text =
            std::fs::read_to_string(dir.join("shaders").join(format!("{name}.metal"))).unwrap();
        ShaderLibrary::from_source(&ctx, name, &text).unwrap()
    };
    let vs = lib("0101010101010101_bt");
    let ds = lib("0303030303030303_b");
    let fs = lib("0202020202020202_b");

    let raw_index_writer = ShaderLibrary::from_source(
        &ctx,
        "tess-test-gpu-index-writer",
        r#"#include <metal_stdlib>
using namespace metal;
kernel void write_indices(device const uchar* input [[buffer(0)]],
                          device uchar* output [[buffer(1)]],
                          uint gid [[thread_position_in_grid]]) {
    if (gid < 14u) output[gid] = input[gid];
}"#,
    )
    .unwrap();
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let writer = cache
        .compute(1, &raw_index_writer, "write_indices")
        .unwrap();
    let fmt = Format::Rgba8Unorm;
    let mut capture_desc = RenderPipelineDesc::new((&vs, "main0_tess_indexed"), None, &[fmt]);
    capture_desc.topology = TopologyClass::Point;
    let capture = cache.render_capture(2, &capture_desc).unwrap();
    let raster = cache
        .render(
            3,
            &RenderPipelineDesc::new(
                (&ds, "main0_tess_indexed_domain"),
                Some((&fs, "main0")),
                &[fmt],
            ),
        )
        .unwrap();
    let tables = |slots| ArgumentTable::new(&ctx, slots, 0).unwrap();
    let vert = tables(31);
    let frag = tables(31);
    let comp = tables(2);

    let w = 64;
    let mut tex = TextureDesc::render_target_2d(w, w, fmt);
    tex.storage = Storage::Shared;
    let target = Texture::new(&ctx, tex).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    let vector = |name| std::fs::read(dir.join("vectors").join(name)).unwrap();
    let vertex_bytes = vector("vertices.bin");
    let uniform_bytes = vector("domain_uniform_slot2.bin");
    for (filename, invalid) in [("indexed.bin", false), ("indexed_invalid_right.bin", true)] {
        let bytes = vector(filename);
        assert_eq!(bytes.len(), 14);
        assert_eq!(vertex_bytes.len(), 224);
        let scratch = Scratch::new(&ctx, 64 * 1024).unwrap();
        let put = |data: &[u8]| scratch.push(data).unwrap();
        let vertices = put(&vertex_bytes);
        let mut layout = [0; crate::pull::BYTES];
        layout[..8].copy_from_slice(&vertices.to_le_bytes());
        layout[8..12].copy_from_slice(&32u32.to_le_bytes());
        for (slot, offset) in [0, 16].into_iter().enumerate() {
            let at = crate::pull::ATTR_OFFSET + slot * 4;
            let attribute = crate::pull::attribute_word(0x2e, offset, 0);
            layout[at..at + 4].copy_from_slice(&attribute.to_le_bytes());
        }
        let mut limits = [0; crate::pull::STREAMS * 8];
        limits[..8].copy_from_slice(&(vertex_bytes.len() as u64).to_le_bytes());
        let source = put(&bytes);
        let indices = put(&[0; 14]);
        let captured = put(&vec![0; CAPTURE_WORDS * 4 * 6]);
        let valid = put(&[0; 6 * 4]);
        let pattern = crate::tess_pattern::pattern([1.; 2], [1.; 4], true).unwrap();
        let coords = put(&coordinate_records(&pattern, 3, 2, 6 * 32).unwrap());
        let ubo = put(&uniform_bytes);
        let domain_args = put(&ubo.to_le_bytes());
        let mut state = vec![0; 48];
        for (dst, value) in state.chunks_exact_mut(4).zip([
            3u32,
            1,
            6,
            0,
            1f32.to_bits(),
            1f32.to_bits(),
            1f32.to_bits(),
            1f32.to_bits(),
            1f32.to_bits(),
            1f32.to_bits(),
        ]) {
            dst.copy_from_slice(&value.to_le_bytes());
        }
        let state = put(&state);
        let mut frame = ring.begin().unwrap();
        let mut writer_pass = frame.commands().begin_compute_pass(&comp).unwrap();
        writer_pass
            .encode(&ComputeCommand::SetPipeline(writer.clone()))
            .unwrap();
        for (index, gpu_addr) in [(0, source), (1, indices)] {
            writer_pass
                .encode(&ComputeCommand::SetBuffer { index, gpu_addr })
                .unwrap();
        }
        writer_pass
            .encode(&ComputeCommand::DispatchThreads {
                grid: [14, 1, 1],
                threads: [14, 1, 1],
            })
            .unwrap();
        writer_pass.end();
        let mut pass = frame
            .commands()
            .begin_render_pass_split(
                &RenderPassDesc {
                    colors: vec![Some(ColorAttachment {
                        texture: target.clone(),
                        depth_plane: 0,
                        load: LoadAction::Clear,
                        store: StoreAction::Store,
                        clear: [0.; 4],
                        memoryless: false,
                    })],
                    depth: None,
                    stencil: None,
                },
                &vert,
                &frag,
            )
            .unwrap();
        Capture {
            pipeline: capture.clone(),
            args: put(&[0; 8]),
            streams: vec![
                (crate::pull::SLOT, put(&layout)),
                (INDEX_SLOT, indices + 2),
                (
                    INDEX_INFO_SLOT,
                    put(&index_descriptor(2, 6, -2, 6).unwrap()),
                ),
                (INDEX_VALID_SLOT, valid),
                (STREAM_LIMIT_SLOT, put(&limits)),
            ],
            descriptor: put(&capture_descriptor(captured, 0, 6)),
            first: 0,
            count: 6,
        }
        .encode(&mut pass)
        .unwrap();
        pass.encode(&RenderCommand::SetPipeline(raster.clone()))
            .unwrap();
        for (index, gpu_addr) in [
            (0, domain_args),
            (DOMAIN_CAPTURE_SLOT, captured),
            (COORD_SLOT, coords),
            (STATE_SLOT, state),
            (DOMAIN_VALID_SLOT, valid),
        ] {
            pass.encode(&RenderCommand::SetBuffer { index, gpu_addr })
                .unwrap();
        }
        pass.encode(&RenderCommand::Draw {
            primitive: Primitive::Triangle,
            first_vertex: 0,
            vertex_count: 6,
            instance_count: 1,
            base_instance: 0,
        })
        .unwrap();
        pass.end();
        frame.submit().unwrap().wait().unwrap();
        let mut pixels = vec![0; (w * w * 4) as usize];
        target.read((w * 4) as usize, &mut pixels).unwrap();
        let check = |x: u32, y: u32, expected: [u8; 4]| {
            let at = ((y * w + x) * 4) as usize;
            let current = &pixels[at..at + 4];
            assert!(
                current
                    .iter()
                    .zip(expected)
                    .all(|(a, b)| a.abs_diff(b) <= 1),
                "pixel ({x},{y}) {current:?} != {expected:?}, invalid={invalid}"
            );
        };
        check(10, 40, [255, 0, 112, 255]);
        check(42, 40, if invalid { [0; 4] } else { [0, 255, 80, 255] });
        check(30, 10, [0; 4]);
    }
}
