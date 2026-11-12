//! The `freshzero` knob: a new Metal texture that gets no upload starts at zero.
//!
//! A Metal texture that the store made new for this frame holds whatever Metal gave it. When its record needs no
//! upload (the GPU wrote the guest copy last, or the texture is scaled or widened, which the plain upload does not
//! handle) the frame uses it as it is. With the knob on, [`Zeroer`] clears every such texture to zero before the frame
//! uses it: one render pass per mip level, layer and depth plane, with the load action clear and the store action
//! store, in the first command buffer of the frame, which holds the uploads. It is a diagnostic, not a fix.
//!
//! A texture that is compressed, has no render target usage, has no colour or depth aspect (stencil only) or is
//! memoryless is skipped and counted.

use super::*;
use nxkit_metal::{
    ArgumentTable, ColorAttachment, DepthAttachment, FormatClass, LoadAction, RenderPassDesc,
    StencilAttachment, Storage, StoreAction, TextureKind, ViewDesc,
};

/// The textures that the knob cleared and skipped in one frame.
pub(super) struct Zeroer {
    on: bool,
    table: Option<ArgumentTable>,
    cleared: u32,
    skipped: u32,
}

impl Zeroer {
    /// A zeroer that does nothing unless `on`.
    pub(super) fn new(on: bool) -> Self {
        Self {
            on,
            table: None,
            cleared: 0,
            skipped: 0,
        }
    }

    /// Clears `res` when it is a new texture without an upload. True when it was cleared: its next reader needs a
    /// barrier from the fragment stage.
    pub(super) fn zero(&mut self, ctx: &Context, cb: &mut CommandBuffer, res: &Res) -> bool {
        // A texture over guest memory holds the game's bytes: clearing it would clear guest memory.
        if !self.on || !res.fresh || res.in_place.is_some() {
            return false;
        }
        let d = res.desc;
        let class = d.format.class();
        if d.storage == Storage::Memoryless
            || !d.render_target
            || d.format.is_compressed()
            || class == FormatClass::Stencil
        {
            self.skipped += 1;
            return false;
        }
        if self.table.is_none() {
            self.table = ArgumentTable::new(ctx, 1, 0).ok();
        }
        let done = self
            .table
            .as_ref()
            .is_some_and(|t| clear_all(cb, t, res).is_ok());
        if done {
            self.cleared += 1;
        } else {
            self.skipped += 1;
        }
        done
    }

    /// Prints the line of the frame, on a sampled frame under `rendercond` or `life`.
    pub(super) fn report(&self) {
        if self.on && crate::diag::sampled() && (crate::diag("rendercond") || crate::diag("life")) {
            eprintln!(
                "DIAG freshzero cleared {} skipped {}",
                self.cleared, self.skipped
            );
        }
    }
}

/// One clear pass for each level, layer and depth plane of `res`.
fn clear_all(cb: &mut CommandBuffer, table: &ArgumentTable, res: &Res) -> Result<()> {
    let d = res.desc;
    let three_d = d.kind == TextureKind::D3;
    for level in 0..d.mip_levels.max(1) {
        let planes = if three_d {
            (d.depth >> level).max(1)
        } else {
            1
        };
        for slice in 0..d.slice_count() {
            let texture = if d.kind == TextureKind::D2 && d.mip_levels <= 1 {
                res.tex.clone()
            } else {
                res.tex.view(&ViewDesc {
                    format: d.format,
                    kind: if three_d {
                        TextureKind::D3
                    } else {
                        TextureKind::D2
                    },
                    first_level: level,
                    level_count: 1,
                    first_slice: slice,
                    slice_count: 1,
                })?
            };
            for plane in 0..planes {
                let mut desc = RenderPassDesc::default();
                if d.format.class() == FormatClass::Color {
                    desc.colors.push(Some(ColorAttachment {
                        texture: texture.clone(),
                        depth_plane: plane,
                        load: LoadAction::Clear,
                        store: StoreAction::Store,
                        clear: [0.0; 4],
                        memoryless: false,
                    }));
                } else {
                    desc.depth = Some(DepthAttachment {
                        texture: texture.clone(),
                        load: LoadAction::Clear,
                        store: StoreAction::Store,
                        clear: 0.0,
                        memoryless: false,
                    });
                    if d.format.has_stencil() {
                        desc.stencil = Some(StencilAttachment {
                            texture: texture.clone(),
                            load: LoadAction::Clear,
                            store: StoreAction::Store,
                            clear: 0,
                            memoryless: false,
                        });
                    }
                }
                cb.begin_render_pass(&desc, table)?.end();
            }
        }
    }
    Ok(())
}
