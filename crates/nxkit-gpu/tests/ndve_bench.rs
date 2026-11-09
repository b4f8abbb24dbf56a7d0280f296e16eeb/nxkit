//! GPU time of the native NDVE movie network at the real size (1920x1080 -> 2560x1440, synthetic weights and picture,
//! no game data). A diagnostic, not a gate: `cargo test --release -p nxkit-gpu --test ndve_bench -- --ignored --nocapture`.
//! Each step is timed alone in its own command buffer (the command buffer's GPU start to end), the whole plan in one
//! command buffer; minimum and median of 30 runs. The picture hashes before and after a change must be equal.
//! `NDVE_BENCH_SRC` names a file with the complete MSL source to use instead of `msl::SOURCE`.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use nxkit_metal::{
    ArchiveConfig, ArgumentTable, ComputeCommand, Context, DataBuffer, Format, FrameRing,
    GuestMemory, PipelineCache, ShaderLibrary, Texture, TextureDesc, TextureRegion,
};
use nxkit_ndve::plan::{GameResource, Plan, Source, Step};
use nxkit_ndve::{kernels, msl};

const PIC: [u32; 2] = [1920, 1080];
const OUT: [u32; 2] = [2560, 1440];
const RUNS: usize = 80;

fn key(name: &str) -> u64 {
    name.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    })
}

fn hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    })
}

fn weights(len: usize, amp: f64) -> Vec<u8> {
    (0..len / 2)
        .flat_map(|i| {
            let h = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 40;
            let v = (h % 2001) as f64 / 1000.0 - 1.0;
            nxkit_ndve::half::from_f64(v * amp).to_le_bytes()
        })
        .collect()
}

fn push_conversion() -> Vec<u8> {
    let mut w = [0f32; 15];
    // BT.709 limited range: rows (y, u, v) for r, g, b; offsets for y, u, v
    w[0..3].copy_from_slice(&[1.1644, 0.0, 1.7927]);
    w[4..7].copy_from_slice(&[1.1644, -0.2132, -0.5329]);
    w[8..11].copy_from_slice(&[1.1644, 2.1124, 0.0]);
    w[12..15].copy_from_slice(&[-16.0 / 255.0, -0.5, -0.5]);
    w.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn push_resampler() -> Vec<u8> {
    let mut w = [0u32; 21];
    w[2] = PIC[0] - 1;
    w[3] = PIC[1] - 1;
    w[10] = (1.0 / OUT[0] as f32).to_bits();
    w[11] = (1.0 / OUT[1] as f32).to_bits();
    w[18] = OUT[1] - 1;
    w[19] = 0;
    w[20] = 1;
    w.iter().flat_map(|v| v.to_le_bytes()).collect()
}

struct Rig {
    ctx: Context,
    table: ArgumentTable,
    ring: FrameRing,
    buffers: Vec<DataBuffer>,
    luma: Texture,
    chroma: Texture,
    dither: Texture,
    picture: Texture,
    out: Texture,
}

fn upload(rig: &mut Rig, guest: &Arc<GuestMemory>, tex: &Texture, bytes: &[u8], w: u32, bpp: u32) {
    guest.write(0, bytes).unwrap();
    let mut frame = rig.ring.begin().unwrap();
    let mut pass = frame.commands().begin_compute_pass(&rig.table).unwrap();
    pass.encode(&ComputeCommand::CopyGuestToTexture {
        guest: guest.clone(),
        offset: 0,
        bytes_per_row: w * bpp,
        bytes_per_image: 0,
        texture: tex.clone(),
        region: TextureRegion::whole(0, 0, [w, (bytes.len() as u32) / (w * bpp), 1]),
    })
    .unwrap();
    pass.end();
    frame.submit().unwrap().wait().unwrap();
}

fn encode(rig: &mut Rig, lib: &ShaderLibrary, cache: &PipelineCache, steps: &[&Step]) -> f64 {
    let mut frame = rig.ring.begin().unwrap();
    let push = [
        frame.scratch().push(&push_conversion()).unwrap(),
        frame.scratch().push(&push_resampler()).unwrap(),
    ];
    {
        let mut pass = frame.commands().begin_compute_pass(&rig.table).unwrap();
        for (n, step) in steps.iter().enumerate() {
            if n > 0 {
                pass.encode(&ComputeCommand::Barrier).unwrap();
            }
            let pipe = cache.compute(key(step.kernel), lib, step.kernel).unwrap();
            pass.encode(&ComputeCommand::SetPipeline(pipe)).unwrap();
            for b in &step.buffers {
                let gpu_addr = match b.source {
                    Source::Buffer(n) => rig.buffers[n].gpu_address(),
                    Source::Game(GameResource::Push(n)) => push[n as usize],
                    s => panic!("buffer source {s:?}"),
                };
                pass.encode(&ComputeCommand::SetBuffer {
                    index: b.index,
                    gpu_addr,
                })
                .unwrap();
            }
            for b in &step.textures {
                let texture = match b.source {
                    Source::Game(GameResource::Texture(0)) => rig.luma.clone(),
                    Source::Game(GameResource::Texture(1)) => rig.chroma.clone(),
                    Source::Game(GameResource::Texture(2)) => rig.dither.clone(),
                    Source::Game(GameResource::Intermediate { index: 1 }) => rig.picture.clone(),
                    Source::Game(GameResource::Output) => rig.out.clone(),
                    s => panic!("texture source {s:?}"),
                };
                pass.encode(&ComputeCommand::SetTexture {
                    index: b.index,
                    texture,
                })
                .unwrap();
            }
            pass.encode(&ComputeCommand::DispatchThreadgroups {
                groups: step.groups,
                threads: step.threads,
            })
            .unwrap();
        }
        pass.end();
    }
    frame.submit().unwrap().wait().unwrap().gpu_ns as f64 / 1e6
}

fn stats(mut t: Vec<f64>) -> (f64, f64) {
    t.sort_by(f64::total_cmp);
    (t[0], t[t.len() / 2])
}

#[test]
#[ignore = "needs a Metal 4 device; a timing diagnostic"]
fn time_every_step() {
    let ctx = Context::system_default().expect("Metal 4 device");
    let source = match std::env::var("NDVE_BENCH_SRC") {
        Ok(f) => std::fs::read_to_string(f).expect("source file"),
        Err(_) => msl::SOURCE.to_string(),
    };
    let lib = ShaderLibrary::from_source(&ctx, "ndve", &source).unwrap();
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let plan: Plan = kernels::plan(&weights(0x3c320, 0.12), &weights(0x3000, 0.1)).expect("plan");
    let buffers: Vec<DataBuffer> = plan
        .buffers
        .iter()
        .map(|b| {
            let d = DataBuffer::new(&ctx, b.bytes).unwrap();
            if let Some(init) = &b.init {
                d.write(0, init).unwrap();
            }
            d
        })
        .collect();
    let tex = |w, h, f| Texture::new(&ctx, TextureDesc::compute_2d(w, h, f)).unwrap();
    let table = ArgumentTable::new(&ctx, 8, 4).unwrap();
    let ring = FrameRing::new(&ctx).unwrap();
    let mut rig = Rig {
        luma: tex(PIC[0], PIC[1], Format::R8Unorm),
        chroma: tex(PIC[0] / 2, PIC[1] / 2, Format::Rg8Unorm),
        dither: tex(512, 512, Format::Rgba8Unorm),
        picture: tex(PIC[0], PIC[1], Format::Rgb10A2Unorm),
        out: tex(OUT[0], OUT[1], Format::Rgba8Unorm),
        ctx,
        table,
        ring,
        buffers,
    };
    let guest = Arc::new(GuestMemory::allocate(&rig.ctx, 8 << 20).unwrap());
    let noise = |i: usize, s: usize| ((i.wrapping_mul(2_654_435_761) >> s) & 0xff) as u8;
    let y: Vec<u8> = (0..(PIC[0] * PIC[1]) as usize)
        .map(|i| {
            let (x, y) = ((i % 1920) as f32, (i / 1920) as f32);
            (110.0 + 60.0 * (x * 0.021 + y * 0.013).sin()) as u8 + (noise(i, 9) & 7)
        })
        .collect();
    let c: Vec<u8> = (0..(PIC[0] * PIC[1] / 2) as usize)
        .map(|i| 112 + (noise(i, 5) & 31))
        .collect();
    let d: Vec<u8> = (0..512 * 512 * 4).map(|i| noise(i, 11)).collect();
    let (luma, chroma, dither) = (rig.luma.clone(), rig.chroma.clone(), rig.dither.clone());
    upload(&mut rig, &guest, &luma, &y, PIC[0], 1);
    upload(&mut rig, &guest, &chroma, &c, PIC[0] / 2, 2);
    upload(&mut rig, &guest, &dither, &d, 512, 4);

    let all: Vec<&Step> = plan.steps.iter().collect();
    let mut totals = Vec::new();
    let mut per_step = vec![Vec::new(); all.len()];
    for it in 0..RUNS + 5 {
        let t = encode(&mut rig, &lib, &cache, &all);
        let each: Vec<f64> = all
            .iter()
            .map(|s| encode(&mut rig, &lib, &cache, &[s]))
            .collect();
        if it >= 5 {
            totals.push(t);
            for (v, e) in per_step.iter_mut().zip(each) {
                v.push(e);
            }
        }
    }
    // one clean full run for the hashes
    encode(&mut rig, &lib, &cache, &all);
    let mut pic = vec![0u8; (PIC[0] * PIC[1] * 4) as usize];
    rig.picture.read((PIC[0] * 4) as usize, &mut pic).unwrap();
    let mut out = vec![0u8; (OUT[0] * OUT[1] * 4) as usize];
    rig.out.read((OUT[0] * 4) as usize, &mut out).unwrap();
    let mut sum = 0.0;
    for (s, v) in all.iter().zip(per_step) {
        let (min, med) = stats(v);
        sum += min;
        println!("BENCH {:14} min {min:6.3} med {med:6.3} ms", s.kernel);
    }
    let (min, med) = stats(totals);
    println!("BENCH sum of step minima {sum:6.3} ms");
    println!("BENCH whole plan, one command buffer: min {min:6.3} med {med:6.3} ms");
    println!(
        "BENCH hash picture {:016x} output {:016x}",
        hash(&pic),
        hash(&out)
    );
    println!(
        "BENCH output pixel (1000, 700): {:?}, distinct bytes {}",
        &out[(700 * OUT[0] as usize + 1000) * 4..][..4],
        out.iter().collect::<std::collections::BTreeSet<_>>().len()
    );
}
