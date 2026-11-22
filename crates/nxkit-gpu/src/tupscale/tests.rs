//! The motion kernel on the GPU against the arithmetic of `math.rs`, and the copy draw between formats.

use super::*;
use crate::settings::VelocityCode;
use make::own;
use math::Rows;
use nxkit_metal::FrameRing;
use run::{copy, motion_pass};

const SIZE: [u32; 2] = [8, 4];

fn texture(ctx: &Context, format: Format, write: bool) -> Texture {
    Texture::new(
        ctx,
        TextureDesc {
            shader_write: write,
            ..own((SIZE[0], SIZE[1]), format)
        },
    )
    .unwrap()
}

fn clear(cb: &mut CommandBuffer, ctx: &Context, t: &Texture, value: [f64; 4]) {
    let table = ArgumentTable::new(ctx, 1, 1).unwrap();
    let depth = t.desc().format.is_depth();
    let desc = RenderPassDesc {
        colors: if depth {
            vec![]
        } else {
            vec![Some(ColorAttachment {
                depth_plane: 0,
                texture: t.clone(),
                load: LoadAction::Clear,
                store: StoreAction::Store,
                clear: value,
                memoryless: false,
            })]
        },
        depth: depth.then(|| nxkit_metal::DepthAttachment {
            texture: t.clone(),
            load: LoadAction::Clear,
            store: StoreAction::Store,
            clear: value[0],
            memoryless: false,
        }),
        stencil: None,
    };
    cb.begin_render_pass(&desc, &table).unwrap().end();
}

fn half(b: [u8; 2]) -> f32 {
    let h = u16::from_le_bytes(b);
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = i32::from((h >> 10) & 0x1f);
    let frac = f32::from(h & 0x3ff);
    sign * match exp {
        0 => frac * 2f32.powi(-24),
        _ => (1.0 + frac / 1024.0) * 2f32.powi(exp - 15),
    }
}

/// Runs the kernel over a depth of `z` and a velocity of `raw` everywhere and gives the motion of `points`.
fn run(
    code: &VelocityCode,
    m: &Rows,
    row_sign: f32,
    z: f64,
    raw: [f64; 2],
) -> Option<Vec<[f32; 2]>> {
    let ctx = Context::system_default().ok()?;
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let lib = ShaderLibrary::from_source(&ctx, msl::LIBRARY, msl::MSL).unwrap();
    let pipe = cache.compute(1, &lib, msl::MOTION).unwrap();
    let depth = texture(&ctx, Format::Depth32FloatStencil8, false);
    let velocity = texture(&ctx, Format::Rg16Unorm, false);
    let motion = texture(&ctx, MOTION_FORMAT, true);
    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut fr = ring.begin().unwrap();
    let scratch = fr.scratch();
    let cb = fr.commands();
    clear(cb, &ctx, &depth, [z; 4]);
    clear(cb, &ctx, &velocity, [raw[0], raw[1], 0.0, 0.0]);
    let params = msl::params(m, SIZE, code, &Signs::PLAIN, row_sign);
    motion_pass(
        &ctx,
        cb,
        &scratch,
        &pipe,
        [&depth, &velocity, &motion],
        &params,
        SIZE,
    )
    .unwrap();
    ctx.commit_residency();
    fr.submit().unwrap().wait().unwrap();
    let px = motion.debug_read_raw(&ctx, &POINTS).unwrap();
    Some(
        px.iter()
            .map(|b| [half([b[0], b[1]]), half([b[2], b[3]])])
            .collect(),
    )
}

const POINTS: [[u32; 2]; 3] = [[0, 0], [7, 3], [3, 1]];

fn code() -> VelocityCode {
    VelocityCode {
        scale: [4.008; 2],
        bias: [-2.003_97; 2],
        y_up: true,
        zero_means_camera: true,
    }
}

fn near(a: [f32; 2], b: [f32; 2]) -> bool {
    (0..2).all(|i| (a[i] - b[i]).abs() <= 0.01 + b[i].abs() * 2e-3)
}

#[test]
fn the_kernel_decodes_a_written_velocity_as_math_does() {
    let raw = [0.6, 0.45];
    let id: Rows = std::array::from_fn(|r| std::array::from_fn(|c| f32::from(u8::from(r == c))));
    let Some(got) = run(&code(), &id, 1.0, 0.5, raw) else {
        eprintln!("skipped: no Metal device");
        return;
    };
    // The texel holds the value rounded to 16 bits.
    let q = raw.map(|v| ((v * 65535.0).round() / 65535.0) as f32);
    let v = math::decode_velocity(q, &code()).unwrap();
    let want = math::motion_pixels(v, SIZE, 1.0, &Signs::PLAIN);
    for g in got {
        assert!(near(g, want), "{g:?} vs {want:?}");
    }
}

#[test]
fn the_kernel_reprojects_the_camera_where_no_velocity_was_written() {
    let mut m: Rows = std::array::from_fn(|r| std::array::from_fn(|c| f32::from(u8::from(r == c))));
    // A turn and a perspective term, so the depth and the pixel position both matter.
    m[0][1] = 0.02;
    m[2][3] = 0.3;
    m[3][0] = 0.1;
    m[3][1] = -0.05;
    for row_sign in [1.0, -1.0] {
        let Some(got) = run(&code(), &m, row_sign, 0.25, [0.0, 0.0]) else {
            eprintln!("skipped: no Metal device");
            return;
        };
        for (p, g) in POINTS.iter().zip(got) {
            let ndc = math::ndc_of(*p, SIZE, row_sign);
            let v = math::camera_velocity(ndc, 0.25, &m).unwrap();
            let want = math::motion_pixels(v, SIZE, row_sign, &Signs::PLAIN);
            assert!(
                near(g, want),
                "{p:?} row_sign {row_sign}: {g:?} vs {want:?}"
            );
        }
    }
}

#[test]
fn the_copy_draw_writes_a_texture_one_to_one_into_another_format() {
    let Ok(ctx) = Context::system_default() else {
        eprintln!("skipped: no Metal device");
        return;
    };
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let lib = ShaderLibrary::from_source(&ctx, msl::LIBRARY, msl::MSL).unwrap();
    let desc = RenderPipelineDesc::new(
        (&lib, msl::COPY_VS),
        Some((&lib, msl::COPY_FS)),
        &[Format::Rgba8Unorm],
    );
    let pipe = cache.render(2, &desc).unwrap();
    let src = texture(&ctx, Format::Bgra8Unorm, false);
    let dst = texture(&ctx, Format::Rgba8Unorm, false);
    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut fr = ring.begin().unwrap();
    let cb = fr.commands();
    clear(cb, &ctx, &src, [0.0, 1.0, 0.0, 1.0]);
    let target = ColorAttachment {
        depth_plane: 0,
        texture: dst.clone(),
        load: LoadAction::Load,
        store: StoreAction::Store,
        clear: [0.0; 4],
        memoryless: false,
    };
    copy(&ctx, cb, &src, &target, &pipe).unwrap();
    ctx.commit_residency();
    fr.submit().unwrap().wait().unwrap();
    for (p, px) in POINTS.iter().zip(dst.debug_read(&ctx, &POINTS).unwrap()) {
        assert!(px[1] > 250 && px[0] < 5 && px[2] < 5, "{p:?}: {px:?}");
    }
}
