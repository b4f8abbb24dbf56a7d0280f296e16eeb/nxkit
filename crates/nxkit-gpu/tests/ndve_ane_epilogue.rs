//! The GPU's share of the Neural Engine path (`ndve_ane_quant` and `ndve_ane_epilogue`) against the CPU reference
//! `nxkit_ndve::ane::epilogue::reference` (steps 1 to 5 of `docs/plans/ndve-ane.md`), on synthetic `final`, `delta` and
//! dither data (no game data). The tensors are ordinary shared textures here; the IOSurface wrapping is the same
//! `R16Float` texture. Needs a Metal 4 device, so it is ignored by default:
//! `cargo test -p nxkit-gpu --test ndve_ane_epilogue -- --ignored --nocapture`.
//!
//! The GPU's sampler has 8 fractional bits of weight and the reference is exact, so a pixel may differ by one step of
//! the 8-bit result; the test allows 2 and counts how many pixels differ at all.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use nxkit_metal::{
    ArchiveConfig, ArgumentTable, ComputeCommand, Context, Format, FrameRing, GuestMemory,
    PipelineCache, ShaderLibrary, Storage, Texture, TextureDesc, TextureRegion,
};
use nxkit_ndve::ane::epilogue::{self, Epilogue};
use nxkit_ndve::half;
use nxkit_ndve::kernels::ane_plans;
use nxkit_ndve::msl;
use nxkit_ndve::plan::{self, GameResource, Source};

const OUT: [u32; 2] = [2560, 1440];

fn key(name: &str) -> u64 {
    name.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    })
}

fn noise(i: usize, s: usize) -> u32 {
    ((i.wrapping_mul(2_654_435_761) >> s) & 0xffff) as u32
}

fn push() -> Vec<u8> {
    let mut w = [0u32; 21];
    w[10] = (1.0 / OUT[0] as f32).to_bits();
    w[11] = (1.0 / OUT[1] as f32).to_bits();
    w[18] = OUT[1] - 1;
    w[19] = 1;
    w[20] = 1;
    w.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn shared(ctx: &Context, w: u32, h: u32, f: Format) -> Texture {
    let mut d = TextureDesc::compute_2d(w, h, f);
    d.storage = Storage::Shared;
    d.shader_write = true;
    d.shader_read = true;
    Texture::new(ctx, d).unwrap()
}

#[test]
#[ignore = "needs a Metal 4 device"]
fn the_epilogue_matches_the_cpu_reference() {
    let ctx = Context::system_default().expect("Metal 4 device");
    let lib = ShaderLibrary::from_source(&ctx, "ndve-ane", msl::SOURCE).unwrap();
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let table = ArgumentTable::new(&ctx, 8, 4).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    let guest = Arc::new(GuestMemory::allocate(&ctx, 40 << 20).unwrap());

    // final: a smooth picture with some noise, three planes of 1080 rows; delta: small values around zero.
    let fin: Vec<u16> = (0..3 * 1920 * 1080)
        .map(|i| {
            let (x, y) = ((i % 1920) as f32, ((i / 1920) % 1080) as f32);
            let v = 0.5 + 0.4 * (x * 0.011 + y * 0.017 + (i / (1920 * 1080)) as f32).sin();
            half::from_f32(v + (noise(i, 7) % 64) as f32 / 4000.0)
        })
        .collect();
    let delta: Vec<u16> = (0..2560 * 1440)
        .map(|i| half::from_f32((noise(i, 3) % 200) as f32 / 4000.0 - 0.025))
        .collect();
    let dith: Vec<u8> = (0..512 * 512 * 4)
        .map(|i| (noise(i, 9) & 0xff) as u8)
        .collect();

    let t_final = shared(&ctx, 1920, 3240, Format::R16Float);
    let t_delta = shared(&ctx, 2560, 1440, Format::R16Float);
    let t_dither = shared(&ctx, 512, 512, Format::Rgba8Unorm);
    let t_pic = shared(&ctx, 1920, 1080, Format::Rgb10A2Unorm);
    let t_out = shared(&ctx, 2560, 1440, Format::Rgba8Unorm);

    let mut upload = |tex: &Texture, bytes: &[u8], w: u32, bpp: u32| {
        guest.write(0, bytes).unwrap();
        let mut frame = ring.begin().unwrap();
        let mut pass = frame.commands().begin_compute_pass(&table).unwrap();
        pass.encode(&ComputeCommand::CopyGuestToTexture {
            guest: guest.clone(),
            offset: 0,
            bytes_per_row: w * bpp,
            bytes_per_image: 0,
            texture: tex.clone(),
            region: TextureRegion::whole(0, 0, [w, bytes.len() as u32 / (w * bpp), 1]),
        })
        .unwrap();
        pass.end();
        frame.submit().unwrap().wait().unwrap();
    };
    let bytes = |v: &[u16]| v.iter().flat_map(|h| h.to_le_bytes()).collect::<Vec<u8>>();
    upload(&t_final, &bytes(&fin), 1920, 2);
    upload(&t_delta, &bytes(&delta), 2560, 2);
    upload(&t_dither, &dith, 512, 4);

    let plans = ane_plans();
    let mut frame = ring.begin().unwrap();
    let push_addr = frame.scratch().push(&push()).unwrap();
    {
        let mut pass = frame.commands().begin_compute_pass(&table).unwrap();
        for (n, step) in plans.epilogue.steps.iter().enumerate() {
            if n > 0 {
                pass.encode(&ComputeCommand::Barrier).unwrap();
            }
            let pipe = cache.compute(key(step.kernel), &lib, step.kernel).unwrap();
            pass.encode(&ComputeCommand::SetPipeline(pipe)).unwrap();
            for b in &step.buffers {
                assert_eq!(b.source, Source::Game(plan::RESAMPLER_PUSH));
                pass.encode(&ComputeCommand::SetBuffer {
                    index: b.index,
                    gpu_addr: push_addr,
                })
                .unwrap();
            }
            for b in &step.textures {
                let Source::Game(g) = b.source else {
                    panic!("texture source {:?}", b.source)
                };
                let texture = match g {
                    g if g == plan::ANE_FINAL => t_final.clone(),
                    g if g == plan::ANE_DELTA => t_delta.clone(),
                    g if g == plan::DITHER => t_dither.clone(),
                    g if g == plan::PICTURE_RGB10A2 => t_pic.clone(),
                    g if g == plan::OUTPUT => t_out.clone(),
                    g => panic!("resource {g:?}"),
                };
                let _: GameResource = g;
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
    frame.submit().unwrap().wait().unwrap();

    let mut got = vec![0u8; (OUT[0] * OUT[1] * 4) as usize];
    t_out.read((OUT[0] * 4) as usize, &mut got).unwrap();
    let e = Epilogue::from_push(&push()).unwrap();
    let want = epilogue::reference(
        &fin,
        &delta,
        &|x, y| {
            let o = 4 * (y * 512 + x);
            [dith[o], dith[o + 1], dith[o + 2]].map(|b| f32::from(b) / 255.0)
        },
        &e,
    );
    let (mut max, mut differing) = (0u8, 0usize);
    for (a, b) in got.iter().zip(&want) {
        let d = a.abs_diff(*b);
        max = max.max(d);
        differing += usize::from(d != 0);
    }
    println!(
        "epilogue: max difference {max}, differing bytes {differing} of {}",
        got.len()
    );
    assert!(max <= 2, "largest difference {max}");
}
