//! The Metal kernels of the vr_light NDSR networks against the CPU reference of `nxkit-ndsr` (on a synthetic texture
//! and weight blob, no game data), and their GPU time at Ocarina of Time's size.
//!
//! Needs a Metal 4 device, so the tests are ignored by default:
//! `cargo test --release -p nxkit-gpu --test ndsr_light_kernel -- --ignored --nocapture`.
//! The input is an RGB9E5 texture as in the game (`a335c1db`'s output). The correctness test compiles the kernels with
//! `NDSR_SOFT_SAMPLER` (plain f32 bilinear, which the reference repeats); the timing uses the hardware sampler.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use nxkit_metal::{
    ArchiveConfig, ArgumentTable, ComputeCommand, Context, DataBuffer, Format, FrameRing,
    GuestMemory, PipelineCache, ShaderLibrary, Texture, TextureDesc, TextureRegion,
};
use nxkit_ndsr::kernels::light::{self, LIGHT_KERNELS, LightSpec};
use nxkit_ndsr::net::{NETS, Net, Pack};
use nxkit_ndsr::plan::{GameResource, Plan, Source};
use nxkit_ndsr::reference::light::{LightPush, run_image};
use nxkit_ndsr::reference::{Picture, Texture as RefTexture};
use nxkit_ndsr::{half, msl};

/// A small deterministic generator (an LCG): values in 0 .. 1.
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (self.0 >> 8) as f32 / (1u32 << 24) as f32
    }
}

fn blob(len: usize, quantised: bool) -> Vec<u8> {
    let mut rng = Rng(7);
    (0..len / 2)
        .flat_map(|_| {
            let mut v = (rng.next() * 2.0 - 1.0) * 0.4;
            if quantised {
                v = (v * 64.0).round() / 64.0;
            }
            half::from_f64(f64::from(v)).to_le_bytes()
        })
        .collect()
}

/// sqrt-RGB values that RGB9E5 holds exactly: multiples of 1/512 below 1.
fn picture(w: u32, h: u32) -> Picture {
    Picture::from_fn(w as usize, h as usize, |x, y| {
        let noise = Rng((x as u32).wrapping_mul(7919) ^ (y as u32).wrapping_mul(104_729)).next();
        let (xf, yf) = (x as f32, y as f32);
        let g = 0.5 + 0.3 * (xf * 0.21 + yf * 0.13).sin() + (noise - 0.5) * 0.16;
        let q = |v: f32| (v.clamp(0.0, 0.998) * 512.0).round() / 512.0;
        [
            q(g + 0.1 * (xf * 0.05).sin()),
            q(g),
            q(g - 0.1 * (yf * 0.07).cos()),
            1.0,
        ]
    })
}

/// RGB9E5 of channels that are multiples of 1/512 below 1: exponent 15, the mantissa is the multiple.
fn rgb9e5(p: &Picture) -> Vec<u8> {
    p.data
        .iter()
        .flat_map(|t| {
            let m = |c: f32| (c * 512.0) as u32;
            (m(t[0]) | m(t[1]) << 9 | m(t[2]) << 18 | 15 << 27).to_le_bytes()
        })
        .collect()
}

/// The kernel's `NDSR_SOFT_SAMPLER` filter in the same f32 operations.
struct SoftPicture(Picture);

impl RefTexture for SoftPicture {
    fn texel(&self, x: i64, y: i64) -> [f32; 4] {
        self.0.texel(x, y)
    }

    fn sample(&self, u: f32, v: f32) -> [f64; 4] {
        let (w, h) = (self.0.width as i64, self.0.height as i64);
        let p = [u * w as f32 - 0.5, v * h as f32 - 0.5];
        let f = [p[0] - p[0].floor(), p[1] - p[1].floor()];
        let at = |dx: i64, dy: i64| {
            self.0.texel(
                (p[0].floor() as i64 + dx).clamp(0, w - 1),
                (p[1].floor() as i64 + dy).clamp(0, h - 1),
            )
        };
        let (a, b, c, d) = (at(0, 0), at(1, 0), at(0, 1), at(1, 1));
        std::array::from_fn(|k| {
            let top = a[k] * (1.0 - f[0]) + b[k] * f[0];
            let bot = c[k] * (1.0 - f[0]) + d[k] * f[0];
            f64::from(top * (1.0 - f[1]) + bot * f[1])
        })
    }
}

/// The game's push for an input of `size_in` and its exact ratio (`w20 = 1`), the tile of `k`, an output offset.
fn push_for(k: &LightSpec, size_in: [u32; 2], offset: [u32; 2], strength: f32) -> LightPush {
    let out = (
        size_in[0] * k.out_px / k.in_px,
        size_in[1] * k.out_px / k.in_px,
    );
    let tile = k.tile();
    LightPush {
        offset: (offset[0] as i32, offset[1] as i32),
        tile: (tile[0], tile[1]),
        scale: [1.0, 1.0],
        origin: [0.0, 0.0],
        out,
        rcp_out: [1.0 / out.0 as f32, 1.0 / out.1 as f32],
        strength,
    }
}

fn net_of(k: &LightSpec) -> &'static Net {
    NETS.iter()
        .find(|n| n.pack == Pack::VrLight && light::find(n) == Some(k))
        .expect("a program")
}

fn pipeline_key(name: &str) -> u64 {
    name.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    })
}

struct Gpu {
    ctx: Context,
    cache: PipelineCache,
    lib: ShaderLibrary,
    table: ArgumentTable,
    ring: FrameRing,
}

impl Gpu {
    fn new(defs: &str) -> Gpu {
        let ctx = Context::system_default().expect("a Metal 4 device");
        let lib =
            ShaderLibrary::from_source(&ctx, "ndsr", &format!("{defs}\n{}", msl::SOURCE)).unwrap();
        let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
        let table = ArgumentTable::new(&ctx, 4, 4).unwrap();
        let ring = FrameRing::new(&ctx).unwrap();
        Gpu {
            ctx,
            cache,
            lib,
            table,
            ring,
        }
    }

    fn texture(&mut self, pic: &Picture) -> Texture {
        let (w, h) = (pic.width as u32, pic.height as u32);
        let guest =
            Arc::new(GuestMemory::allocate(&self.ctx, u64::from(w * h * 4).max(1 << 20)).unwrap());
        guest.write(0, &rgb9e5(pic)).unwrap();
        let tex = Texture::new(
            &self.ctx,
            TextureDesc::compute_2d(w, h, Format::Rgb9E5Float),
        )
        .unwrap();
        let mut frame = self.ring.begin().unwrap();
        let mut pass = frame.commands().begin_compute_pass(&self.table).unwrap();
        pass.encode(&ComputeCommand::CopyGuestToTexture {
            guest,
            offset: 0,
            bytes_per_row: w * 4,
            bytes_per_image: 0,
            texture: tex.clone(),
            region: TextureRegion::whole(0, 0, [w, h, 1]),
        })
        .unwrap();
        pass.end();
        frame.submit().unwrap().wait().unwrap();
        tex
    }

    /// Runs the plan once; the GPU time in ms.
    fn run(&mut self, plan: &Plan, words: &[u32], tex0: &Texture, out: &Texture) -> f64 {
        let buffers: Vec<DataBuffer> = plan
            .buffers
            .iter()
            .map(|b| {
                let d = DataBuffer::new(&self.ctx, b.bytes).unwrap();
                d.write(0, b.init.as_ref().unwrap()).unwrap();
                d
            })
            .collect();
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let mut frame = self.ring.begin().unwrap();
        let push = frame.scratch().push(&bytes).unwrap();
        {
            let mut pass = frame.commands().begin_compute_pass(&self.table).unwrap();
            let step = &plan.steps[0];
            let pipe = self
                .cache
                .compute(pipeline_key(step.kernel), &self.lib, step.kernel)
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
                threads: step.threads,
            })
            .unwrap();
            pass.end();
        }
        frame.submit().unwrap().wait().unwrap().gpu_ns as f64 / 1e6
    }
}

/// Runs every kernel on the GPU and on the CPU and compares the pictures (written as f32). All kernels are checked
/// and printed, then the test fails once with the list of those beyond their limits.
///
/// - Exact inputs (`quantised`): every sum of the network is exact, so the kernel's `hstep` (f32 products, then f16)
///   and the reference's single rounding agree, and the f16 epilogue is the same operations. A pixel counts as equal
///   within 1e-5: variant A's sRGB encode uses the fast `exp2` / `log2` (the game's MUFU; a few f32 ulps, the 4.8e-7
///   of the first run), as in `ndsr_kernel.rs`.
/// - Random weights: a K = 16 sum that the GPU's f32 accumulation rounds to the other f16 neighbour changes a grid cell
///   by one f16 step, and with it the cells downstream in the patch. The light epilogue keeps that in f16 and the
///   sharpening (strength 2.5 here: centre 1.25, the neighbours -0.3125 each) spreads it to the four neighbours, so a
///   few per cent of the pixels move by a few f16 steps. Modelled on the CPU for `r12_a`'s first threadgroup
///   (`scratchpad/ndsr/light/flip2.py`: the reference with each HMMA as sequential f32 fmas): 336 of 2304 cells and
///   2.7 % of the pixels beyond 4e-3, largest 0.025; with exact inputs none. The GPU gave 3.7 % and 0.050.
fn compare(quantised: bool) {
    let mut failed = Vec::new();
    let mut gpu = Gpu::new("#define NDSR_SOFT_SAMPLER");
    const IN: [u32; 2] = [100, 70];
    const OFFSET: [u32; 2] = [3, 1];
    let pic = picture(IN[0], IN[1]);
    let tex0 = gpu.texture(&pic);
    let soft = SoftPicture(pic);
    for k in &LIGHT_KERNELS {
        let net = net_of(k);
        let blob = blob(k.layers as usize * 0x800, quantised);
        let push = push_for(k, IN, OFFSET, 2.5);
        let words = push.words(IN);
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let plan = light::plan(net, &blob, IN, &bytes).expect("plan");
        let size = [push.out.0 + OFFSET[0], push.out.1 + OFFSET[1]];
        let out = Texture::new(
            &gpu.ctx,
            TextureDesc::compute_2d(size[0], size[1], Format::Rgba32Float),
        )
        .unwrap();
        gpu.run(&plan, &words, &tex0, &out);
        let mut got = vec![0u8; (size[0] * size[1] * 16) as usize];
        out.read((size[0] * 16) as usize, &mut got).unwrap();
        let want = run_image(net, &blob, &push, &soft, IN).expect("reference");
        let (mut equal, mut max, mut beyond) = (0usize, 0f32, 0usize);
        for y in 0..want.height {
            for x in 0..want.width {
                let v = want.rgba[y * want.width + x];
                assert!(!v[0].is_nan(), "{}: ({x},{y}) unwritten", k.name);
                let at =
                    ((y + OFFSET[1] as usize) * size[0] as usize + x + OFFSET[0] as usize) * 16;
                let d = (0..3)
                    .map(|c| {
                        let g =
                            f32::from_le_bytes(got[at + 4 * c..at + 4 * c + 4].try_into().unwrap());
                        (g - v[c]).abs()
                    })
                    .fold(0.0, f32::max);
                max = max.max(d);
                equal += usize::from(d <= 1e-5);
                beyond += usize::from(d > 4e-3);
            }
        }
        let total = want.width * want.height;
        eprintln!(
            "{} ({}): {total} pixels, {equal} equal (1e-5), {beyond} beyond 4e-3, largest {max}",
            k.name, net.key
        );
        let ok = if quantised {
            equal * 100 >= total * 99 && max <= 1e-2
        } else {
            beyond * 20 <= total && max <= 0.125
        };
        if !ok {
            failed.push(k.name);
        }
    }
    assert!(failed.is_empty(), "beyond the limits: {failed:?}");
}

#[test]
#[ignore = "needs a Metal 4 device"]
fn the_light_kernels_equal_the_reference_on_exact_inputs() {
    compare(true);
}

#[test]
#[ignore = "needs a Metal 4 device"]
fn the_light_kernels_follow_the_reference_on_random_weights() {
    compare(false);
}

/// GPU time of every vr_light kernel at 1536x864 in, each at its own ratio (Ocarina of Time's network
/// `ndsr_light_r43_b`: 2048x1152, R11G11B10F). Minimum and median of 25 runs.
#[test]
#[ignore = "needs a Metal 4 device; a timing diagnostic"]
fn time_every_light_kernel() {
    const IN: [u32; 2] = [1536, 864];
    let mut gpu = Gpu::new("");
    let tex0 = gpu.texture(&picture(IN[0], IN[1]));
    let only = std::env::var("NDSR_BENCH_ONLY").unwrap_or_default();
    for k in LIGHT_KERNELS.iter().filter(|k| k.name.contains(&only)) {
        let push = push_for(k, IN, [0, 0], 2.5);
        let words = push.words(IN);
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let blob = blob(k.layers as usize * 0x800, false);
        let plan = light::plan(net_of(k), &blob, IN, &bytes).expect("plan");
        let fmt = if k.srgb {
            Format::Rgba8Unorm
        } else {
            Format::Rg11B10Float
        };
        let out = Texture::new(
            &gpu.ctx,
            TextureDesc::compute_2d(push.out.0, push.out.1, fmt),
        )
        .unwrap();
        let mut t: Vec<f64> = (0..30)
            .map(|_| gpu.run(&plan, &words, &tex0, &out))
            .skip(5)
            .collect();
        t.sort_by(f64::total_cmp);
        println!(
            "BENCH {:18} {}x{}  min {:6.3} med {:6.3}",
            k.name,
            push.out.0,
            push.out.1,
            t[0],
            t[t.len() / 2]
        );
    }
}
