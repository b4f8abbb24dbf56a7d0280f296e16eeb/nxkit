//! An argument table does not own the textures that it binds: the command buffer does. A table that lives for the
//! whole run must not keep a texture alive after the GPU is done with it. Without a Metal device the test prints a
//! message and passes.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use nxkit_metal::{
    ArchiveConfig, ArgumentTable, ComputeCommand, Context, DataBuffer, Format, FrameRing,
    PipelineCache, ShaderLibrary, Texture, TextureDesc, mem_stats,
};

const SAMPLE_MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;
kernel void sample_one(device float4* out [[buffer(0)]], texture2d<float> tex [[texture(0)]]) {
    constexpr sampler s(coord::normalized, filter::nearest);
    out[0] = tex.sample(s, float2(0.5));
}
"#;

#[test]
fn a_long_lived_table_does_not_keep_the_textures_it_bound() {
    let ctx = match Context::system_default() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skipped: no usable Metal 4 device ({e})");
            return;
        }
    };
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let lib = ShaderLibrary::from_source(&ctx, "table_retain", SAMPLE_MSL).unwrap();
    let pipeline = cache.compute(1, &lib, "sample_one").unwrap();
    let table = ArgumentTable::new(&ctx, 1, 1).unwrap();
    let output = DataBuffer::new(&ctx, 16).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    // The counts before the first texture. Everything the test needs besides the textures exists by now.
    let before = mem_stats();

    for _ in 0..5 {
        let texture = Texture::new(&ctx, TextureDesc::compute_2d(64, 64, Format::R8Unorm)).unwrap();
        ctx.commit_residency();
        let mut frame = ring.begin().unwrap();
        {
            let mut pass = frame.commands().begin_compute_pass(&table).unwrap();
            pass.encode(&ComputeCommand::SetPipeline(pipeline.clone()))
                .unwrap();
            pass.encode(&ComputeCommand::SetBuffer {
                index: 0,
                gpu_addr: output.gpu_address(),
            })
            .unwrap();
            pass.encode(&ComputeCommand::SetTexture {
                index: 0,
                texture: texture.clone(),
            })
            .unwrap();
            pass.encode(&ComputeCommand::DispatchThreads {
                grid: [1; 3],
                threads: [1; 3],
            })
            .unwrap();
            pass.end();
        }
        let submitted = frame.submit().unwrap();
        submitted.wait().unwrap();
        drop(submitted);
        drop(texture);
    }
    // The ring may hold the last command buffers, and with them what they retained.
    drop(ring);

    let after = mem_stats();
    assert_eq!(after.textures, before.textures);
    assert_eq!(after.texture_bytes, before.texture_bytes);
}
