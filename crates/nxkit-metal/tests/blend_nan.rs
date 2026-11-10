//! A blend term with factor 0 and a NaN or Inf source colour.
//!
//! On the console a blend term whose factor is exactly 0 contributes 0, whatever the other operand is. A pixel
//! shader can compute `Inf * 0 = NaN` in its colour and write it with alpha 0 under `SRC_ALPHA` /
//! `ONE_MINUS_SRC_ALPHA`; the console keeps the destination. The question here is what this GPU does with
//! `0 x NaN`, in a float target, and whether a shader that zeroes the colour when the source factor is 0 restores
//! the console result.
//!
//! - `premise_zero_factor_keeps_nan` draws ten cases into `Rgba16Float` and `Rg11B10Float` targets, prints the
//!   raw bits and the decoded values of every case and asserts the cases that decide the premise.
//! - `the_rule_restores_the_console_result` draws four of them again through a fragment function that applies the
//!   rule and asserts the destination colour exactly.
//!
//! Measured on an M2 Max, the same in `Rgba16Float` and `Rg11B10Float` (D is the destination colour):
//!
//! | case | source | blend (src / dst) | stored RGB |
//! |---|---|---|---|
//! | A | NaN, alpha 0 | `SRC_ALPHA` / `ONE_MINUS_SRC_ALPHA` | NaN |
//! | B | +Inf, alpha 0 | same | NaN |
//! | C | -Inf, alpha 0 | same | NaN |
//! | D | NaN, alpha 1e-10 | same | NaN |
//! | E | NaN, alpha 1 | `ZERO` / `ONE` | D |
//! | F | NaN, alpha 1 | `ONE_MINUS_SRC_ALPHA` / `SRC_ALPHA` | NaN |
//! | G | NaN, alpha 0.5 | `BLEND_COLOR` (0) / `ONE` | NaN |
//! | H | (1,1,1,1) over a NaN destination | `SRC_ALPHA` / `ONE_MINUS_SRC_ALPHA` | NaN |
//! | I | NaN, alpha 1, blend off | none | NaN |
//! | J | (2,3,4), alpha 0 | `SRC_ALPHA` / `ONE_MINUS_SRC_ALPHA` | D |
//!
//! So `0 x NaN` and `0 x Inf` are NaN in the blender, except for a `ZERO` factor, which keeps the destination. A
//! NaN or Inf source colour with alpha 0 under `SRC_ALPHA` / `ONE_MINUS_SRC_ALPHA` stores NaN, where the console
//! keeps the destination. The rule (zero the colour when the source factor is 0) fixes A, B and the like. It does
//! not cover a NaN destination (H) or a zero blend colour (G), because the shader sees neither. A GPU on which A
//! and B do not store NaN needs no rule; the first test then fails and documents that.
//!
//! The colour comes as bits from a buffer, so the compiler cannot fold NaN or Inf away.
//!
//! Needs a Metal 4 GPU. Without one the tests print a message and pass.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use nxkit_metal::{
    ArchiveConfig, ArgumentTable, BlendFactor, BlendOp, BlendState, ColorAttachment, ColorTarget,
    Context, DataBuffer, Format, FrameRing, LoadAction, PipelineCache, Primitive, RenderCommand,
    RenderPassDesc, RenderPipelineDesc, ShaderLibrary, Storage, StoreAction, Texture, TextureDesc,
};

const MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct V { float4 pos [[position]]; };
vertex V vs_full(uint id [[vertex_id]]) {
    float2 p = float2((id << 1) & 2, id & 2);
    V v; v.pos = float4(p * 2.0 - 1.0, 0.5, 1.0); return v;
}

// The colour comes as bits from a buffer. The second word is the kind of the rule.
struct P { uint4 src; uint kind; };

fragment float4 fs_bits(V in [[stage_in]], constant P& p [[buffer(0)]]) {
    return as_type<float4>(p.src);
}

// The rule: when the source RGB factor is exactly 0, the colour is 0.
static inline __attribute__((always_inline)) bool dm_bz_zero(uint k, float a, float a1)
{
    uint b = k < 4u ? as_type<uint>(a) : as_type<uint>(a1);
    return k == 1u || ((k == 2u || k == 4u) && (b & 0x7fffffffu) == 0u) || ((k == 3u || k == 5u) && b == 0x3f800000u);
}

fragment float4 fs_rule(V in [[stage_in]], constant P& p [[buffer(0)]]) {
    float4 c = as_type<float4>(p.src);
    if (dm_bz_zero(p.kind & 7u, c.w, 0.0f)) { c.x = 0.0f; c.y = 0.0f; c.z = 0.0f; }
    return c;
}
"#;

const NAN: u32 = 0x7fc0_0000;
const INF: u32 = 0x7f80_0000;
const NEG_INF: u32 = 0xff80_0000;
const ONE: u32 = 0x3f80_0000;
/// 1e-10.
const TINY: u32 = 0x2edb_e6ff;
/// 0.5.
const HALF: u32 = 0x3f00_0000;
/// The destination colour. Exact in both formats.
const D: [f32; 4] = [0.25, 0.5, 0.75, 1.0];

fn ctx() -> Option<Context> {
    match Context::system_default() {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!("skipped: no usable Metal 4 device ({e})");
            None
        }
    }
}

fn shared_target(ctx: &Context, format: Format) -> Texture {
    let mut d = TextureDesc::render_target_2d(4, 4, format);
    d.storage = Storage::Shared;
    Texture::new(ctx, d).unwrap()
}

fn alpha_blend() -> BlendState {
    BlendState {
        src_rgb: BlendFactor::SrcAlpha,
        dst_rgb: BlendFactor::OneMinusSrcAlpha,
        op_rgb: BlendOp::Add,
        src_alpha: BlendFactor::One,
        dst_alpha: BlendFactor::Zero,
        op_alpha: BlendOp::Add,
    }
}

fn factors(src: BlendFactor, dst: BlendFactor) -> BlendState {
    BlendState {
        src_rgb: src,
        dst_rgb: dst,
        ..alpha_blend()
    }
}

/// One draw: the source bits, the blend state (none for replacement) and the rule kind.
#[derive(Clone)]
struct Draw {
    src: [u32; 4],
    blend: Option<BlendState>,
    kind: u32,
}

/// A case: its name, its draws in order, and the constant blend colour set before the first draw.
struct Case {
    name: &'static str,
    draws: Vec<Draw>,
    blend_color: Option<[f32; 4]>,
}

fn rgb(v: u32, a: u32) -> [u32; 4] {
    [v, v, v, a]
}

fn draw(src: [u32; 4], blend: Option<BlendState>, kind: u32) -> Draw {
    Draw { src, blend, kind }
}

fn single(name: &'static str, d: Draw) -> Case {
    Case {
        name,
        draws: vec![d],
        blend_color: None,
    }
}

/// The ten cases of the premise. The kind is the one the rule needs for the source factor of the blend.
fn cases() -> Vec<Case> {
    let over = Some(alpha_blend());
    vec![
        single("A nan a0 src_alpha", draw(rgb(NAN, 0), over, 2)),
        single("B +inf a0 src_alpha", draw(rgb(INF, 0), over, 2)),
        single("C -inf a0 src_alpha", draw(rgb(NEG_INF, 0), over, 2)),
        single("D nan a1e-10 src_alpha", draw(rgb(NAN, TINY), over, 2)),
        single(
            "E nan a1 zero/one",
            draw(
                rgb(NAN, ONE),
                Some(factors(BlendFactor::Zero, BlendFactor::One)),
                1,
            ),
        ),
        single(
            "F nan a1 1-src_alpha/src_alpha",
            draw(
                rgb(NAN, ONE),
                Some(factors(
                    BlendFactor::OneMinusSrcAlpha,
                    BlendFactor::SrcAlpha,
                )),
                3,
            ),
        ),
        Case {
            name: "G nan a.5 blend_color(0)/one",
            draws: vec![draw(
                rgb(NAN, HALF),
                Some(factors(BlendFactor::BlendColor, BlendFactor::One)),
                0,
            )],
            blend_color: Some([0.0; 4]),
        },
        Case {
            name: "H nan destination, dst factor 0",
            draws: vec![draw(rgb(NAN, ONE), None, 0), draw(rgb(ONE, ONE), over, 0)],
            blend_color: None,
        },
        single("I nan a1 blend off", draw(rgb(NAN, ONE), None, 0)),
        single(
            "J finite (2,3,4) a0 src_alpha",
            draw(
                [2.0f32.to_bits(), 3.0f32.to_bits(), 4.0f32.to_bits(), 0],
                over,
                2,
            ),
        ),
    ]
}

/// Draws a case into a fresh target of `format`, cleared to `D`, and returns the texel (1, 1) as raw bytes.
fn run(
    ctx: &Context,
    cache: &PipelineCache,
    lib: &ShaderLibrary,
    format: Format,
    case: &Case,
    fs: &str,
) -> Vec<u8> {
    static KEY: AtomicU64 = AtomicU64::new(1000);
    let target = shared_target(ctx, format);
    let desc = RenderPassDesc {
        colors: vec![Some(ColorAttachment {
            depth_plane: 0,
            texture: target.clone(),
            load: LoadAction::Clear,
            store: StoreAction::Store,
            clear: D.map(f64::from),
            memoryless: false,
        })],
        depth: None,
        stencil: None,
    };
    // One buffer per draw: 16 bytes of source bits, then the kind, padded to 32.
    let buffers: Vec<DataBuffer> = case
        .draws
        .iter()
        .map(|d| {
            let b = DataBuffer::new(ctx, 32).unwrap();
            let mut bytes = [0u8; 32];
            for (i, w) in d.src.iter().enumerate() {
                bytes[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
            }
            bytes[16..20].copy_from_slice(&d.kind.to_le_bytes());
            b.write(0, &bytes).unwrap();
            b
        })
        .collect();
    let pipelines: Vec<_> = case
        .draws
        .iter()
        .map(|d| {
            let mut pd = RenderPipelineDesc::new((lib, "vs_full"), Some((lib, fs)), &[format]);
            pd.colors = vec![Some(ColorTarget {
                format,
                blend: d.blend,
                write_mask: 0xF,
            })];
            cache
                .render(KEY.fetch_add(1, Ordering::Relaxed), &pd)
                .unwrap()
        })
        .collect();
    let (vt, ft) = (
        ArgumentTable::new(ctx, 1, 1).unwrap(),
        ArgumentTable::new(ctx, 1, 1).unwrap(),
    );
    let mut ring = FrameRing::new(ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    {
        let cb = frame.commands();
        let mut pass = cb.begin_render_pass_split(&desc, &vt, &ft).unwrap();
        if let Some(c) = case.blend_color {
            pass.encode(&RenderCommand::SetBlendColor(c)).unwrap();
        }
        for (p, b) in pipelines.into_iter().zip(&buffers) {
            pass.encode(&RenderCommand::SetPipeline(p)).unwrap();
            pass.encode(&RenderCommand::SetFragmentBuffer {
                index: 0,
                gpu_addr: b.gpu_address(),
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
    ctx.commit_residency();
    frame.submit().unwrap().wait().unwrap();

    let bpp = if format == Format::Rgba16Float { 8 } else { 4 };
    let mut px = vec![0u8; 4 * 4 * bpp];
    target.read(4 * bpp, &mut px).unwrap();
    let i = (4 + 1) * bpp;
    px[i..i + bpp].to_vec()
}

/// A small float with a 5-bit exponent (bias 15) and `mbits` mantissa bits.
fn small_float(negative: bool, exp: u32, mant: u32, mbits: u32) -> f32 {
    let s = if negative { -1.0 } else { 1.0 };
    let top = (1u32 << mbits) as f32;
    if exp == 31 {
        return if mant == 0 {
            s * f32::INFINITY
        } else {
            f32::NAN
        };
    }
    if exp == 0 {
        return s * (mant as f32 / top) * 2f32.powi(-14);
    }
    s * (1.0 + mant as f32 / top) * 2f32.powi(exp as i32 - 15)
}

/// The raw bits of the texel as text, and the decoded RGB.
fn decode(format: Format, px: &[u8]) -> (String, [f32; 3]) {
    if format == Format::Rgba16Float {
        let h: Vec<u32> = px
            .chunks(2)
            .map(|c| u32::from(u16::from_le_bytes([c[0], c[1]])))
            .collect();
        let v = |i: usize| small_float(h[i] >> 15 != 0, (h[i] >> 10) & 31, h[i] & 0x3ff, 10);
        let raw = format!("{:04x} {:04x} {:04x} {:04x}", h[0], h[1], h[2], h[3]);
        (raw, [v(0), v(1), v(2)])
    } else {
        let w = u32::from_le_bytes([px[0], px[1], px[2], px[3]]);
        let r = small_float(false, (w >> 6) & 31, w & 0x3f, 6);
        let g = small_float(false, (w >> 17) & 31, (w >> 11) & 0x3f, 6);
        let b = small_float(false, (w >> 27) & 31, (w >> 22) & 0x1f, 5);
        (format!("{w:08x}"), [r, g, b])
    }
}

fn non_finite(v: [f32; 3]) -> bool {
    v.iter().any(|x| !x.is_finite())
}

const FORMATS: [Format; 2] = [Format::Rgba16Float, Format::Rg11B10Float];

fn setup(ctx: &Context) -> (PipelineCache, ShaderLibrary) {
    (
        PipelineCache::new(ctx, &ArchiveConfig::default()).unwrap(),
        ShaderLibrary::from_source(ctx, "blend_nan", MSL).unwrap(),
    )
}

#[test]
fn premise_zero_factor_keeps_nan() {
    let Some(ctx) = ctx() else { return };
    let (cache, lib) = setup(&ctx);
    let mut got = HashMap::new();
    eprintln!("case | format | raw bits | decoded rgb");
    for format in FORMATS {
        for case in cases() {
            let px = run(&ctx, &cache, &lib, format, &case, "fs_bits");
            let (raw, v) = decode(format, &px);
            eprintln!("{} | {:?} | {} | {:?}", case.name, format, raw, v);
            got.insert((case.name.as_bytes()[0] as char, format), v);
        }
    }
    for format in FORMATS {
        for c in ['A', 'B'] {
            let v = got[&(c, format)];
            assert!(
                non_finite(v),
                "case {c} in {format:?} stored {v:?}: this GPU already gives the console result"
            );
        }
        for c in ['C', 'D', 'F', 'G', 'H'] {
            let v = got[&(c, format)];
            assert!(
                v.iter().all(|x| x.is_nan()),
                "case {c} in {format:?} stored {v:?}, measured on an M2 Max: all NaN"
            );
        }
        let e = got[&('E', format)];
        assert_eq!(
            e,
            [D[0], D[1], D[2]],
            "case E in {format:?}: a ZERO source factor keeps the destination"
        );
        let i = got[&('I', format)];
        assert!(
            i.iter().all(|x| x.is_nan()),
            "control I in {format:?}: blend off stored {i:?}, the format cannot hold NaN"
        );
        let j = got[&('J', format)];
        assert_eq!(
            j,
            [D[0], D[1], D[2]],
            "control J in {format:?}: a finite colour with alpha 0 changed the destination"
        );
    }
}

#[test]
fn the_rule_restores_the_console_result() {
    let Some(ctx) = ctx() else { return };
    let (cache, lib) = setup(&ctx);
    for format in FORMATS {
        for case in cases() {
            if !matches!(case.name.as_bytes()[0], b'A' | b'B' | b'E' | b'F') {
                continue;
            }
            let px = run(&ctx, &cache, &lib, format, &case, "fs_rule");
            let (raw, v) = decode(format, &px);
            eprintln!("rule: {} | {:?} | {} | {:?}", case.name, format, raw, v);
            assert_eq!(
                v,
                [D[0], D[1], D[2]],
                "case {} in {format:?} with the rule: {raw}",
                case.name
            );
        }
    }
}

/// The multiply blend: source factor `ZERO`, destination factor `SRC_COLOR`. The destination factor reads the
/// source colour, so the rule must not change it. D x 0.5 is exact in both formats, so the compare is exact.
#[test]
fn the_rule_must_not_touch_a_colour_that_the_destination_factor_reads() {
    let Some(ctx) = ctx() else { return };
    let (cache, lib) = setup(&ctx);
    let multiply = Some(factors(BlendFactor::Zero, BlendFactor::SrcColor));
    for format in FORMATS {
        let case = |kind| single("K multiply", draw(rgb(HALF, ONE), multiply, kind));
        // Kind 0, as the host sets it for this blend: the destination times the source colour.
        let px = run(&ctx, &cache, &lib, format, &case(0), "fs_rule");
        let (raw, v) = decode(format, &px);
        eprintln!("multiply kind 0 | {format:?} | {raw} | {v:?}");
        assert_eq!(v, [D[0] * 0.5, D[1] * 0.5, D[2] * 0.5], "{format:?}");
        // Kind 1 zeroes the source colour and so the product. It documents why the host must not set it here.
        let px = run(&ctx, &cache, &lib, format, &case(1), "fs_rule");
        let (raw, v) = decode(format, &px);
        eprintln!("multiply kind 1 | {format:?} | {raw} | {v:?}");
        assert_eq!(v, [0.0; 3], "{format:?}");
    }
}
