//! GPU time of the 15 NDSR kernels on the full-size picture 1536x864 -> 1920x1080 (the real texture sampler, no game
//! data). A diagnostic, not a gate: `cargo test --release -p nxkit-gpu --test ndsr_bench -- --ignored --nocapture`.
//! Each kernel is timed alone in its own command buffer (the command buffer's GPU start to end); the median of 15.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use nxkit_metal::{
    ArchiveConfig, ArgumentTable, ComputeCommand, Context, DataBuffer, Format, FrameRing,
    GuestMemory, PipelineCache, ShaderLibrary, Texture, TextureDesc, TextureRegion,
};
use nxkit_ndsr::kernels::{self, KERNELS};
use nxkit_ndsr::plan::{GameResource, Source};
use nxkit_ndsr::{msl, pack};

const IN: [u32; 2] = [1536, 864];
const OUT: [u32; 2] = [1920, 1080];

fn push_words() -> Vec<u8> {
    let mut w = [0u32; 20];
    w[2] = IN[0] - 1;
    w[3] = IN[1] - 1;
    w[6] = OUT[0] - 1;
    w[7] = OUT[1] - 1;
    w[8] = (1.0 / IN[0] as f32).to_bits();
    w[9] = (1.0 / IN[1] as f32).to_bits();
    w[18] = 0.5f32.to_bits();
    w[19] = 1;
    w.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn key(name: &str) -> u64 {
    name.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    })
}

#[test]
#[ignore = "needs a Metal 4 device; a timing diagnostic"]
fn time_every_kernel() {
    let ctx = Context::system_default().expect("Metal 4 device");
    // NDSR_BENCH_SRC: comma-separated files holding complete MSL sources, timed side by side (round-robin per
    // iteration, so that load from other work hits all alike); default is msl::SOURCE. NDSR_BENCH_DEFS is put before
    // every source, e.g. "#define NDSR_SOFT_SAMPLER" (what ndsr_kernel.rs runs). NDSR_BENCH_ONLY keeps the kernels
    // whose name contains it.
    let defs = std::env::var("NDSR_BENCH_DEFS").unwrap_or_default();
    let sources: Vec<String> = match std::env::var("NDSR_BENCH_SRC") {
        Ok(list) => list
            .split(',')
            .map(|f| std::fs::read_to_string(f).expect("source file"))
            .collect(),
        Err(_) => vec![msl::SOURCE.to_string()],
    };
    let libs: Vec<ShaderLibrary> = sources
        .iter()
        .map(|src| ShaderLibrary::from_source(&ctx, "ndsr", &format!("{defs}\n{src}")).unwrap())
        .collect();
    let caches: Vec<PipelineCache> = libs
        .iter()
        .map(|_| PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap())
        .collect();
    let only = std::env::var("NDSR_BENCH_ONLY").unwrap_or_default();
    // NDSR_BENCH_THREADS: threads of a threadgroup for each source (default: the plan's)
    let threads: Vec<Option<u32>> = std::env::var("NDSR_BENCH_THREADS")
        .map(|l| l.split(',').map(|t| t.parse().ok()).collect())
        .unwrap_or_default();
    let blob: Vec<u8> = (0..pack::BLOB_BYTES / 2)
        .flat_map(|i| {
            let v = ((i * 2_654_435_761usize >> 7) % 1000) as f64 / 1000.0 - 0.5;
            nxkit_ndsr::half::from_f64(v * 0.3).to_le_bytes()
        })
        .collect();
    let guest = Arc::new(GuestMemory::allocate(&ctx, 32 << 20).unwrap());
    let texels: Vec<u8> = (0..IN[0] * IN[1])
        .flat_map(|i| {
            let (x, y) = ((i % IN[0]) as f32, (i / IN[0]) as f32);
            let g = 0.5 + 0.3 * (x * 0.021 + y * 0.013).sin();
            [g + 0.05, g, g - 0.05, 1.0]
                .into_iter()
                .flat_map(f32::to_le_bytes)
        })
        .collect();
    guest.write(0, &texels).unwrap();
    let tex0 = Texture::new(
        &ctx,
        TextureDesc::compute_2d(IN[0], IN[1], Format::Rgba32Float),
    )
    .unwrap();
    let table = ArgumentTable::new(&ctx, 4, 4).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    {
        let mut frame = ring.begin().unwrap();
        let mut pass = frame.commands().begin_compute_pass(&table).unwrap();
        pass.encode(&ComputeCommand::CopyGuestToTexture {
            guest: guest.clone(),
            offset: 0,
            bytes_per_row: IN[0] * 16,
            bytes_per_image: 0,
            texture: tex0.clone(),
            region: TextureRegion::whole(0, 0, [IN[0], IN[1], 1]),
        })
        .unwrap();
        pass.end();
        frame.submit().unwrap().wait().unwrap();
    }
    for k in KERNELS.iter().filter(|k| k.name.contains(&only)) {
        let plan = kernels::plan_kernel(k, &blob, IN, OUT).expect("plan");
        let buffers: Vec<DataBuffer> = plan
            .buffers
            .iter()
            .map(|b| {
                let d = DataBuffer::new(&ctx, b.bytes).unwrap();
                d.write(0, b.init.as_ref().unwrap()).unwrap();
                d
            })
            .collect();
        let fmt = if k.name.ends_with('c') {
            Format::Rg11B10Float
        } else {
            Format::Rgba32Float
        };
        let out = Texture::new(&ctx, TextureDesc::compute_2d(OUT[0], OUT[1], fmt)).unwrap();
        let mut times = vec![Vec::new(); libs.len()];
        for it in 0..30 {
            for (v, lib) in libs.iter().enumerate() {
                let mut frame = ring.begin().unwrap();
                let push = frame.scratch().push(&push_words()).unwrap();
                {
                    let mut pass = frame.commands().begin_compute_pass(&table).unwrap();
                    let step = &plan.steps[0];
                    let pipe = caches[v]
                        .compute(key(step.kernel), lib, step.kernel)
                        .unwrap();
                    pass.encode(&ComputeCommand::SetPipeline(pipe)).unwrap();
                    for b in &step.buffers {
                        let gpu_addr = match b.source {
                            Source::Buffer(n) => buffers[n].gpu_address(),
                            _ => push,
                        };
                        pass.encode(&ComputeCommand::SetBuffer {
                            index: b.index,
                            gpu_addr,
                        })
                        .unwrap();
                    }
                    for b in &step.textures {
                        let texture = match b.source {
                            Source::Game(GameResource::Texture(0)) => tex0.clone(),
                            _ => out.clone(),
                        };
                        pass.encode(&ComputeCommand::SetTexture {
                            index: b.index,
                            texture,
                        })
                        .unwrap();
                    }
                    pass.encode(&ComputeCommand::DispatchThreadgroups {
                        groups: step.groups,
                        threads: [
                            threads.get(v).copied().flatten().unwrap_or(step.threads[0]),
                            1,
                            1,
                        ],
                    })
                    .unwrap();
                    pass.end();
                }
                let stats = frame.submit().unwrap().wait().unwrap();
                if it >= 5 {
                    times[v].push(stats.gpu_ns as f64 / 1e6);
                }
            }
        }
        // the hash of what the last variant wrote: the same bits mean the same kernel
        let bpp = if fmt == Format::Rg11B10Float { 4 } else { 16 };
        let mut px = vec![0u8; (OUT[0] * OUT[1] * bpp) as usize];
        out.read((OUT[0] * bpp) as usize, &mut px).unwrap();
        let hash = px.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
            (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
        });
        let mut line = format!("BENCH {:12}", k.name);
        for t in &mut times {
            t.sort_by(f64::total_cmp);
            line += &format!("  min {:6.3} med {:6.3}", t[0], t[t.len() / 2]);
        }
        println!("{line}  hash {hash:016x}");
    }
}
