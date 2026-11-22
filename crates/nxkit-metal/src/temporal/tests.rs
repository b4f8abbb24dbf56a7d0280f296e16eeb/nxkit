use super::*;
use crate::render::{ColorAttachment, DepthAttachment, RenderPassDesc};
use crate::{ArgumentTable, LoadAction, Storage, StoreAction, TextureDesc, TextureKind};

fn desc() -> TemporalDesc {
    TemporalDesc {
        input: (32, 32),
        output: (64, 64),
        color: Format::Rgba8Unorm,
        depth: Format::Depth32Float,
        motion: Format::Rg16Float,
        output_format: Format::Rgba8Unorm,
        reactive: None,
        hdr: false,
    }
}

fn texture(ctx: &Context, size: (u32, u32), format: Format, write: bool) -> Texture {
    Texture::new(
        ctx,
        TextureDesc {
            kind: TextureKind::D2,
            width: size.0,
            height: size.1,
            depth: 1,
            mip_levels: 1,
            array_layers: 1,
            allow_views: false,
            format,
            shader_read: true,
            shader_write: write,
            render_target: true,
            storage: Storage::Private,
            shader_atomic: false,
        },
    )
    .unwrap()
}

fn clear(ctx: &Context, cmd: &mut CommandBuffer, t: &Texture, rgba: [f64; 4]) {
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
                clear: rgba,
                memoryless: false,
            })]
        },
        depth: depth.then(|| DepthAttachment {
            texture: t.clone(),
            load: LoadAction::Clear,
            store: StoreAction::Store,
            clear: rgba[0],
            memoryless: false,
        }),
        stencil: None,
    };
    cmd.begin_render_pass(&desc, &table).unwrap().end();
}

fn frame<'a>(color: &'a Texture, depth: &'a Texture, motion: &'a Texture) -> TemporalFrame<'a> {
    TemporalFrame {
        color,
        depth,
        motion,
        reactive: None,
        content: desc().input,
        jitter: (0.0, 0.0),
        motion_scale: (1.0, 1.0),
        depth_reversed: true,
        reset: true,
    }
}

#[test]
fn a_size_of_zero_or_a_colour_depth_is_refused_before_metal_is_asked() {
    let Ok(ctx) = Context::system_default() else {
        eprintln!("skipped: no Metal device");
        return;
    };
    let mut d = desc();
    d.input = (0, 32);
    assert!(TemporalUpscaler::new(&ctx, d).is_err());
    let mut d = desc();
    d.depth = Format::R32Float;
    assert!(TemporalUpscaler::new(&ctx, d).is_err());
}

#[test]
fn a_still_picture_comes_out_at_the_output_size() {
    let Ok(ctx) = Context::system_default() else {
        eprintln!("skipped: no Metal device");
        return;
    };
    if !TemporalUpscaler::supported(&ctx) {
        eprintln!("skipped: no MetalFX temporal scaler");
        return;
    }
    let d = desc();
    let fx = TemporalUpscaler::new(&ctx, d).unwrap();
    let color = texture(&ctx, d.input, d.color, false);
    let depth = texture(&ctx, d.input, d.depth, false);
    let motion = texture(&ctx, d.input, d.motion, false);
    let output = texture(&ctx, d.output, d.output_format, true);
    let mut ring = crate::FrameRing::new(&ctx).unwrap();
    for i in 0..4 {
        let mut fr = ring.begin().unwrap();
        let cmd = fr.commands();
        clear(&ctx, cmd, &color, [0.0, 1.0, 0.0, 1.0]);
        clear(&ctx, cmd, &depth, [0.5; 4]);
        clear(&ctx, cmd, &motion, [0.0; 4]);
        let f = TemporalFrame {
            jitter: [(0.25, -0.25), (-0.25, 0.25)][i % 2],
            reset: i == 0,
            ..frame(&color, &depth, &motion)
        };
        fx.encode(cmd, &f, &output).unwrap();
        fr.submit().unwrap().wait().unwrap();
    }
    let pts = [[0, 0], [63, 0], [0, 63], [63, 63], [32, 32]];
    for (p, px) in pts.iter().zip(output.debug_read(&ctx, &pts).unwrap()) {
        assert!(px[1] > 200 && px[0] < 40 && px[2] < 40, "{p:?}: {px:?}");
    }
}

#[test]
fn a_frame_that_does_not_fit_the_description_is_refused() {
    let Ok(ctx) = Context::system_default() else {
        eprintln!("skipped: no Metal device");
        return;
    };
    if !TemporalUpscaler::supported(&ctx) {
        eprintln!("skipped: no MetalFX temporal scaler");
        return;
    }
    let d = desc();
    let fx = TemporalUpscaler::new(&ctx, d).unwrap();
    let color = texture(&ctx, d.input, d.color, false);
    let depth = texture(&ctx, d.input, d.depth, false);
    let motion = texture(&ctx, d.input, d.motion, false);
    let small = texture(&ctx, d.input, d.output_format, true);
    let out = texture(&ctx, d.output, d.output_format, true);
    let mut ring = crate::FrameRing::new(&ctx).unwrap();
    let mut fr = ring.begin().unwrap();
    let f = frame(&color, &depth, &motion);
    assert!(fx.encode(fr.commands(), &f, &small).is_err(), "output size");
    let wide = TemporalFrame {
        content: (33, 32),
        ..frame(&color, &depth, &motion)
    };
    assert!(fx.encode(fr.commands(), &wide, &out).is_err(), "content");
}
