//! The Metal kernel of the NDSR network against the CPU reference of `nxkit-ndsr`, on a synthetic texture and a
//! synthetic weight blob (no game data). The model is the scratch harness `gpucmp.py` of the recovery.
//!
//! Needs a Metal 4 device, so the test is ignored by default: `cargo test -p nxkit-gpu --test ndsr_kernel -- --ignored`.
//! The kernel is compiled with `NDSR_SOFT_SAMPLER`, which replaces the texture unit's filter by plain f32 bilinear, so
//! that the reference can be matched; the hardware filter's sub-texel precision is a property of the game's GPU.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use nxkit_metal::{
    ArchiveConfig, ArgumentTable, ComputeCommand, Context, DataBuffer, Format, FrameRing,
    GuestMemory, PipelineCache, ShaderLibrary, Texture, TextureDesc, TextureRegion,
};
use nxkit_ndsr::net::{Net, Variant};
use nxkit_ndsr::plan::{GameResource, Source};
use nxkit_ndsr::reference::{self, Picture, Push, Texture as RefTexture, Weights};
use nxkit_ndsr::{kernels, msl};

const IN: [u32; 2] = [100, 70];
const OUT: [u32; 2] = [150, 90];
const OFFSET: [u32; 2] = [5, 2];
const STRENGTH: f32 = 0.5;
/// The largest difference in the output of variants A and B (linear or sRGB value, 0 .. 1) that passes.
const TOLERANCE: f32 = 2e-3;
/// The largest difference of any pixel on random weights.
const LIMIT: f32 = 3e-2;

/// A small deterministic generator (an LCG): values in 0 .. 1.
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (self.0 >> 8) as f32 / (1u32 << 24) as f32
    }
}

fn blob(quantised: bool) -> Vec<u8> {
    let mut rng = Rng(7);
    (0..0x1800)
        .flat_map(|_| {
            let mut v = (rng.next() * 2.0 - 1.0) * 0.4;
            if quantised {
                v = (v * 64.0).round() / 64.0;
            }
            nxkit_ndsr::half::from_f64(f64::from(v)).to_le_bytes()
        })
        .collect()
}

fn picture(quantised: bool) -> Picture {
    Picture::from_fn(IN[0] as usize, IN[1] as usize, |x, y| {
        let noise = Rng((x as u32).wrapping_mul(7919) ^ (y as u32).wrapping_mul(104_729)).next();
        let (x, y) = (x as f32, y as f32);
        let mut g = 0.5 + 0.3 * (x * 0.21 + y * 0.13).sin() + (noise - 0.5) * 0.16;
        if quantised {
            g = (g * 256.0).round() / 256.0;
        }
        [
            g + 0.1 * (x * 0.05).sin(),
            g,
            g - 0.1 * (y * 0.07).cos(),
            1.0,
        ]
    })
}

/// The texture filter of the kernel built with `NDSR_SOFT_SAMPLER`, in the same f32 operations: the reference's own
/// sampler works in doubles, which differs in the last bits and would hide whether the kernel is right.
struct SoftPicture(Picture);

impl RefTexture for SoftPicture {
    fn texel(&self, x: i64, y: i64) -> [f32; 4] {
        self.0.texel(x, y)
    }

    fn sample(&self, u: f32, v: f32) -> [f64; 4] {
        let (w, h) = (self.0.width as i64, self.0.height as i64);
        let p = [u * w as f32 - 0.5, v * h as f32 - 0.5];
        let (fl, f) = (
            p.map(f32::floor),
            [p[0] - p[0].floor(), p[1] - p[1].floor()],
        );
        let at = |dx: i64, dy: i64| {
            self.0.texel(
                (fl[0] as i64 + dx).clamp(0, w - 1),
                (fl[1] as i64 + dy).clamp(0, h - 1),
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

fn push_words() -> Vec<u8> {
    let mut w = [0u32; 20];
    w[2] = IN[0] - 1;
    w[3] = IN[1] - 1;
    w[4] = OFFSET[0];
    w[5] = OFFSET[1];
    w[6] = OUT[0] - 1;
    w[7] = OUT[1] - 1;
    w[8] = (1.0 / IN[0] as f32).to_bits();
    w[9] = (1.0 / IN[1] as f32).to_bits();
    w[18] = STRENGTH.to_bits();
    w[19] = 1;
    w.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn format_of(v: Variant) -> Format {
    match v {
        Variant::A | Variant::B => Format::Rgba32Float,
        Variant::C => Format::Rg11B10Float,
    }
}

/// The unsigned float field of `v` with `mant` mantissa bits and 5 exponent bits, rounded to nearest.
fn small_float(v: f32, mant: u32) -> i64 {
    let v = v.max(0.0);
    let max = (0x1e << mant) | ((1 << mant) - 1);
    let field = if v < 2f32.powi(-14) {
        (f64::from(v) * f64::from(1u32 << (14 + mant))).round() as i64
    } else {
        let bits = i64::from(v.to_bits()) - (112 << 23);
        let drop = 23 - mant;
        (bits + (1 << (drop - 1))) >> drop
    };
    field.clamp(0, i64::from(max))
}

/// The R11G11B10F fields the image stores for the reference's value.
fn expected_c(rgb: [f32; 3]) -> [i64; 3] {
    [
        small_float(rgb[0], 6),
        small_float(rgb[1], 6),
        small_float(rgb[2], 5),
    ]
}

/// The fields of an R11G11B10F pixel.
fn stored_c(px: &[u8]) -> [i64; 3] {
    let w = u32::from_le_bytes([px[0], px[1], px[2], px[3]]);
    [w & 0x7ff, (w >> 11) & 0x7ff, w >> 22].map(i64::from)
}

/// The three channels of an Rgba32Float pixel.
fn stored_f(px: &[u8]) -> [f32; 3] {
    std::array::from_fn(|c| {
        f32::from_le_bytes([px[4 * c], px[4 * c + 1], px[4 * c + 2], px[4 * c + 3]])
    })
}

/// The cache key of a kernel: one pipeline per name.
fn pipeline_key(name: &str) -> u64 {
    name.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    })
}

struct Gpu {
    ctx: Context,
    cache: PipelineCache,
    lib: ShaderLibrary,
}

/// Runs the plan of `net` and returns the pixels of the output texture, rows of `OUT + OFFSET`.
fn run_on_gpu(gpu: &Gpu, net: &Net, blob: &[u8], pic: &Picture) -> Vec<u8> {
    let plan = kernels::plan(net, blob, IN, OUT, &push_words()).expect("plan");
    let ctx = &gpu.ctx;
    let buffers: Vec<DataBuffer> = plan
        .buffers
        .iter()
        .map(|b| {
            let d = DataBuffer::new(ctx, b.bytes).unwrap();
            if let Some(init) = &b.init {
                d.write(0, init).unwrap();
            }
            d
        })
        .collect();

    let mib = 1u64 << 20;
    let guest = Arc::new(GuestMemory::allocate(ctx, mib).unwrap());
    let texels: Vec<u8> = pic
        .data
        .iter()
        .flat_map(|t| t.iter().flat_map(|c| c.to_le_bytes()))
        .collect();
    guest.write(0, &texels).unwrap();
    let tex0 = Texture::new(
        ctx,
        TextureDesc::compute_2d(IN[0], IN[1], Format::Rgba32Float),
    )
    .unwrap();
    let out_size = [OUT[0] + OFFSET[0], OUT[1] + OFFSET[1]];
    let fmt = format_of(net.variant);
    let out = Texture::new(ctx, TextureDesc::compute_2d(out_size[0], out_size[1], fmt)).unwrap();

    let table = ArgumentTable::new(ctx, 4, 4).unwrap();
    let mut ring = FrameRing::new(ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    let push = frame.scratch().push(&push_words()).unwrap();
    {
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
    }
    {
        let mut pass = frame.commands().begin_compute_pass(&table).unwrap();
        pass.encode(&ComputeCommand::WaitForEarlierPasses).unwrap();
        for step in &plan.steps {
            let pipe = gpu
                .cache
                .compute(pipeline_key(step.kernel), &gpu.lib, step.kernel)
                .unwrap();
            pass.encode(&ComputeCommand::SetPipeline(pipe)).unwrap();
            for b in &step.buffers {
                let gpu_addr = match b.source {
                    Source::Buffer(n) => buffers[n].gpu_address(),
                    Source::Game(GameResource::Push(_)) => push,
                    Source::Game(g) => panic!("{g:?} is no buffer here"),
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
                    Source::Game(GameResource::Output) => out.clone(),
                    s => panic!("{s:?} is no texture here"),
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
    frame.submit().unwrap().wait().unwrap();
    let bpp = fmt.bytes_per_pixel() as u32;
    let mut bytes = vec![0u8; (out_size[0] * out_size[1] * bpp) as usize];
    out.read((out_size[0] * bpp) as usize, &mut bytes).unwrap();
    bytes
}

/// One network per input scale, and every variant at least once.
const KEYS: [&str; 6] = [
    "14c10aa70b81f04e", // in 10, A
    "f407ab4429b8eae6", // in 10, B
    "7a5a51af95b27ae5", // in 8, C
    "c16ce0c879c981a3", // in 7, A
    "17c6bd35f159a957", // in 6, B
    "aec4dfce7ab3cefa", // in 4, C
];

fn gpu() -> Gpu {
    let Ok(ctx) = Context::system_default() else {
        panic!("no usable Metal 4 device");
    };
    let source = format!("#define NDSR_SOFT_SAMPLER\n{}", msl::SOURCE);
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let lib = ShaderLibrary::from_source(&ctx, "ndsr", &source).unwrap();
    Gpu { ctx, cache, lib }
}

/// Runs every network of [`KEYS`] on the GPU and on the CPU and compares the pictures. Variants A and B are written
/// as f32 and compared in value; variant C is compared in the fields of the packed image. With `quantised` inputs the sums are exact and the pictures agree; with random ones a few pixels may
/// differ by up to [`TOLERANCE`] and none by more than [`LIMIT`].
fn compare(quantised: bool) {
    let gpu = gpu();
    let (blob, pic) = (blob(quantised), picture(quantised));
    assert!(Weights::from_vr_blob(&blob).is_some());
    let soft = SoftPicture(pic.clone());
    let push = Push {
        w_in: IN[0],
        h_in: IN[1],
        w_out: OUT[0],
        h_out: OUT[1],
        offset: (OFFSET[0] as i32, OFFSET[1] as i32),
        unsharp: STRENGTH,
        dither: true,
    };
    for key in KEYS {
        let (_, net) = Net::by_key(key).expect("net");
        let want = reference::run_image(net, &blob, &push, &soft).expect("reference");
        let got = run_on_gpu(&gpu, net, &blob, &pic);
        let is_c = net.variant == Variant::C;
        let bpp = if is_c { 4 } else { 16 };
        let pitch = (OUT[0] + OFFSET[0]) as usize * bpp;
        let (mut equal, mut near, mut far) = (0usize, 0usize, 0usize);
        let mut max = 0f32;
        for y in 0..OUT[1] as usize {
            for x in 0..OUT[0] as usize {
                let v = want.rgba[y * want.width + x];
                assert!(
                    !v[0].is_nan(),
                    "{key}: the reference leaves ({x},{y}) unwritten"
                );
                let at = (y + OFFSET[1] as usize) * pitch + (x + OFFSET[0] as usize) * bpp;
                let d = if is_c {
                    let e = expected_c([v[0], v[1], v[2]]);
                    let g = stored_c(&got[at..at + 4]);
                    (0..3)
                        .map(|c| (e[c] - g[c]).abs() as f32)
                        .fold(0.0, f32::max)
                } else {
                    let g = stored_f(&got[at..at + 16]);
                    (0..3).map(|c| (g[c] - v[c]).abs()).fold(0.0, f32::max)
                };
                // C counts fields (0 or 1 apart), A and B values.
                let (same, tol) = if is_c { (0.0, 1.0) } else { (1e-5, TOLERANCE) };
                max = max.max(d);
                equal += usize::from(d <= same);
                near += usize::from(d > same && d <= tol);
                far += usize::from(d > tol);
            }
        }
        let total = (OUT[0] * OUT[1]) as usize;
        eprintln!(
            "{key} in {} {:?}: {total} pixels, {equal} equal, {near} within tolerance, {far} beyond, largest {max}",
            net.in_px, net.variant
        );
        if quantised {
            // Weights and input that every sum of the network keeps exact: the same bits, except for a few grid
            // cells where `hstep` (two f32 products, then f16) and the reference (one exact rounding) meet a
            // double-rounding case. Variant C is one unit apart in most pixels (the hardware's conversion rounds differently).
            let limit = if is_c { 16.0 } else { 5e-3 };
            let nearly = if is_c { far * 10 } else { near + far };
            assert!(
                nearly * 100 <= total && max <= limit,
                "{key}: differs by {max}"
            );
        } else {
            // Random weights: a sum that rounds differently once changes a grid cell by an f16 step, which the square
            // root of dark cells magnifies. Rare and small, but not zero (`hstep` against the exact HMMA).
            assert!(
                far * 200 <= total,
                "{key}: {far} of {total} pixels beyond the tolerance"
            );
            assert!(
                max <= if is_c { 64.0 } else { LIMIT },
                "{key}: differs by {max}"
            );
        }
    }
}

#[test]
#[ignore = "needs a Metal 4 device"]
fn the_kernel_equals_the_reference_on_exact_inputs() {
    compare(true);
}

#[test]
#[ignore = "needs a Metal 4 device"]
fn the_kernel_follows_the_reference_on_random_weights() {
    compare(false);
}
