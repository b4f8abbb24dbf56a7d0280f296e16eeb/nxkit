//! The commands on whole images that run as render passes: the clears of images and the resolves of multisample
//! images (the reference layer's clears of images and `resolve_image`).

use nxkit_metal::{
    ColorAttachment, CommandBuffer, ComputeCommand, DepthAttachment, LoadAction, RenderCommand,
    RenderPassDesc, StencilAttachment, StoreAction, Texture, TextureKind, TextureRegion, ViewDesc,
};
use nxkit_vk::cmd::Cmd;
use nxkit_vk::format::{ClearColor, aspect};

use super::Replay;
use super::transfer::texture;

/// `vkCmdClearColorImage` and `vkCmdClearDepthStencilImage`: one render pass per level and layer that clears it.
pub(super) fn clear_image(r: &mut Replay<'_>, cb: &mut CommandBuffer, c: &Cmd) {
    let (image, ranges, color, depth, stencil) = match c {
        Cmd::ClearColorImage {
            image,
            color,
            ranges,
        } => (image, ranges, Some(*color), 0.0, 0),
        Cmd::ClearDepthStencilImage {
            image,
            depth,
            stencil,
            ranges,
        } => (image, ranges, None, *depth, *stencil),
        _ => return,
    };
    let Ok(tex) = texture(r, image) else {
        return;
    };
    let d = tex.desc();
    if d.kind == TextureKind::D3 || !d.render_target {
        r.log.once(
            "clear image",
            "[RED] a clear of a 3D image or of an image that is no transfer destination is not implemented",
        );
        return;
    }
    let f = nxkit_vk::format::lookup(image.format);
    for range in ranges {
        for level in range.base_level..range.base_level + range.levels {
            for layer in range.base_layer..range.base_layer + range.layers {
                let v = ViewDesc {
                    format: d.format,
                    kind: TextureKind::D2,
                    first_level: level,
                    level_count: 1,
                    first_slice: layer,
                    slice_count: 1,
                };
                let Ok(view) = tex.view(&v) else {
                    continue;
                };
                let mut desc = RenderPassDesc::default();
                match color {
                    Some(raw) => {
                        let clear = match f.map(|f| f.clear_color(raw)) {
                            Some(ClearColor::UInt(c)) => c.map(f64::from),
                            Some(ClearColor::SInt(c)) => c.map(f64::from),
                            Some(ClearColor::Float(c)) => c.map(f64::from),
                            None => [0.0; 4],
                        };
                        desc.colors.push(Some(ColorAttachment {
                            texture: view,
                            depth_plane: 0,
                            load: LoadAction::Clear,
                            store: StoreAction::Store,
                            clear,
                            memoryless: false,
                        }));
                    }
                    None => {
                        let act = |bit: u32| {
                            if range.aspect & bit != 0 {
                                LoadAction::Clear
                            } else {
                                LoadAction::Load
                            }
                        };
                        if d.format.is_depth() {
                            desc.depth = Some(DepthAttachment {
                                texture: view.clone(),
                                load: act(aspect::DEPTH),
                                store: StoreAction::Store,
                                clear: f64::from(depth),
                                memoryless: false,
                            });
                        }
                        if d.format.has_stencil() {
                            desc.stencil = Some(StencilAttachment {
                                texture: view,
                                load: act(aspect::STENCIL),
                                store: StoreAction::Store,
                                clear: stencil,
                                memoryless: false,
                            });
                        }
                    }
                }
                match cb.begin_render_pass(&desc, &r.tables.vertex) {
                    Ok(mut p) => {
                        let _ = p.encode(&RenderCommand::WaitForEarlierPasses);
                        p.end();
                        r.stats.passes += 1;
                    }
                    Err(e) => {
                        r.log.once(
                            "clear image pass",
                            format_args!("[RED] a clear of an image: {e}"),
                        );
                    }
                }
            }
        }
    }
}

/// A single-sample view of one level and layer, or of one layer of a multisample texture.
fn layer_view(t: &Texture, level: u32, layer: u32) -> nxkit_metal::Result<Texture> {
    t.view(&ViewDesc {
        format: t.desc().format,
        kind: TextureKind::D2,
        first_level: level,
        level_count: 1,
        first_slice: layer,
        slice_count: 1,
    })
}

/// `vkCmdResolveImage` (the reference layer's `resolve_image`): a pass that loads the multisample layer and resolves
/// it at its end. A region that is not the whole layer resolves into a texture of its own, and a copy takes the region
/// from it.
pub(super) fn resolve_image(r: &mut Replay<'_>, cb: &mut CommandBuffer, c: &Cmd) {
    let Cmd::ResolveImage { src, dst, regions } = c else {
        return;
    };
    let (Ok(a), Ok(b)) = (texture(r, src), texture(r, dst)) else {
        return;
    };
    if nxkit_vk::format::lookup(src.format).is_none_or(|f| !f.is_color() || f.is_int()) {
        r.log.once(
            "resolve format",
            format_args!(
                "[RED] vkCmdResolveImage of format {} is not implemented",
                src.format
            ),
        );
        return;
    }
    for g in regions {
        for l in 0..g.src_sub.layers.max(1) {
            if let Err(e) = resolve_layer(r, cb, &a, &b, g, l) {
                r.log.once(
                    format!("resolve {e}"),
                    format_args!("[RED] vkCmdResolveImage: {e}"),
                );
            }
        }
    }
}

fn resolve_layer(
    r: &mut Replay<'_>,
    cb: &mut CommandBuffer,
    a: &Texture,
    b: &Texture,
    g: &nxkit_vk::cmd::ImageCopy,
    l: u32,
) -> Result<(), String> {
    let sv = layer_view(a, g.src_sub.level, g.src_sub.base_layer + l).map_err(|e| e.to_string())?;
    let dv = layer_view(b, g.dst_sub.level, g.dst_sub.base_layer + l).map_err(|e| e.to_string())?;
    let (sd, dd) = (sv.desc(), dv.desc());
    let whole = g.src_offset == [0; 3]
        && g.dst_offset == [0; 3]
        && g.extent[0] == sd.width
        && g.extent[1] == sd.height
        && (sd.width, sd.height) == (dd.width, dd.height);
    let target = if whole {
        dv
    } else {
        let td = nxkit_metal::TextureDesc::render_target_2d(sd.width, sd.height, sd.format);
        Texture::new(r.ctx, td).map_err(|e| e.to_string())?
    };
    let desc = RenderPassDesc {
        colors: vec![Some(ColorAttachment {
            texture: sv,
            depth_plane: 0,
            load: LoadAction::Load,
            store: StoreAction::Store,
            clear: [0.0; 4],
            memoryless: false,
        })],
        depth: None,
        stencil: None,
    };
    let resolves = [Some(target.clone())];
    let mut p = cb
        .begin_render_pass_resolved(&desc, &r.tables.vertex, &r.tables.fragment, &resolves)
        .map_err(|e| e.to_string())?;
    let _ = p.encode(&RenderCommand::WaitForEarlierPasses);
    p.end();
    r.stats.passes += 1;
    if whole {
        return Ok(());
    }
    let mut pass = cb
        .begin_compute_pass(&r.tables.compute)
        .map_err(|e| e.to_string())?;
    let _ = pass.encode(&ComputeCommand::WaitForEarlierPasses);
    let o = |v: [i32; 3]| [v[0].max(0) as u32, v[1].max(0) as u32, 0];
    let copy = pass.encode(&ComputeCommand::CopyTexture {
        src: target,
        dst: b.clone(),
        src_region: TextureRegion {
            level: 0,
            slice: 0,
            origin: o(g.src_offset),
            size: [g.extent[0], g.extent[1], 1],
        },
        dst_level: g.dst_sub.level,
        dst_slice: g.dst_sub.base_layer + l,
        dst_origin: o(g.dst_offset),
    });
    pass.end();
    r.stats.copies += 1;
    copy.map_err(|e| e.to_string())
}
