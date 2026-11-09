//! Rendering into a compressed texture: the block shadows.
//!
//! A game encodes block compressed textures on the GPU by rendering into them through a view of an unsigned integer
//! format of the block size, so that one texel of the view is one block (agl's BC4 encoder renders a 64x64x64 BC4
//! texture plane by plane through an `RGBA16Uint` view). Metal can neither render into a compressed format nor make an
//! uncompressed view of one. Such an attachment renders into a block shadow instead: a 2D texture of the view format,
//! one texel per block of one level and layer (or depth plane). The encoder copies the shadow into the blocks of the
//! compressed texture after the pass, through a buffer, and the other way before a pass that loads the attachment.

use super::*;

/// A colour attachment that renders the blocks of one level and layer of a compressed texture.
#[derive(Clone)]
pub(crate) struct BlockTarget {
    /// The uncompressed texture that the pass renders into, one texel per block.
    pub(crate) shadow: Texture,
    /// The compressed texture.
    pub(crate) target: Texture,
    /// The box of `target` that the shadow holds: one level, one array slice or one depth plane.
    pub(crate) region: TextureRegion,
    /// The bytes of one row of blocks.
    pub(crate) row_bytes: u32,
}

impl BlockTarget {
    /// The bytes of the blocks in a tightly packed buffer.
    pub(crate) fn bytes(&self) -> u64 {
        let d = self.shadow.desc();
        u64::from(self.row_bytes) * u64::from(d.height)
    }
}

/// The view format through which an attachment renders the blocks of a compressed texture of `format`: an uncompressed
/// override with the size of one block. `None` when the view has no format or another one.
pub(crate) fn block_view_format(format: Format, view: Option<i32>) -> Option<Format> {
    let f = view.and_then(texture_format)?.format();
    (format.is_compressed() && !f.is_compressed() && f.block_bytes() == format.block_bytes())
        .then_some(f)
}

/// The description of the shadow of `level` of a compressed texture `d` for the view format `format`: a render
/// target of one level and slice with one texel per block.
pub(crate) fn shadow_desc(d: &TextureDesc, level: u32, format: Format) -> TextureDesc {
    let (bw, bh) = d.format.block_dim();
    let [w, h, _] = d.level_extent(level);
    TextureDesc {
        kind: TextureKind::D2,
        width: w.div_ceil(bw),
        height: h.div_ceil(bh),
        depth: 1,
        mip_levels: 1,
        array_layers: 1,
        allow_views: false,
        format,
        shader_read: true,
        shader_write: false,
        render_target: true,
        storage: Storage::Private,
        shader_atomic: false,
    }
}

/// The box of a compressed texture `d` that the attachment of `level` and `layer` covers: an array slice, or the depth
/// plane `layer` of a 3D texture.
pub(crate) fn block_region(d: &TextureDesc, level: u32, layer: u32) -> TextureRegion {
    let [w, h, _] = d.level_extent(level);
    if d.kind == TextureKind::D3 {
        TextureRegion {
            level,
            slice: 0,
            origin: [0, 0, layer],
            size: [w, h, 1],
        }
    } else {
        TextureRegion::whole(level, layer, [w, h, 1])
    }
}

impl TextureStore {
    /// The block shadow of an attachment of the compressed texture `res`. The shadows are kept by texture, level and
    /// layer, so a pass that renders a plane never waits for the copy of another plane.
    pub(crate) fn block_target(
        &self,
        res: &Res,
        view: nxkit_nvn::frame::TargetView,
    ) -> Result<BlockTarget, Skip> {
        let d = res.desc;
        let refuse = |why: &str| {
            let what = format_args!("block target {view:?} of {d:?}: {why}");
            nxkit_nvn::diag::note(nxkit_nvn::diag::Reason::ViewCreate, what);
            crate::tex_skip(what)
        };
        let Some(format) = block_view_format(d.format, view.format()) else {
            return Err(refuse(
                "the view is not an uncompressed format of the block size",
            ));
        };
        let (level, layer) = (view.level(), view.layer());
        let depth = if d.kind == TextureKind::D3 {
            d.level_extent(level)[2]
        } else {
            d.slice_count()
        };
        if level >= d.mip_levels || layer >= depth || res.scaled {
            return Err(refuse("the level or layer is outside the texture"));
        }
        let sd = shadow_desc(&d, level, format);
        let key = (res.generation, level, layer, format_key(format));
        let mut shadows = self.shadows.lock().unwrap_or_else(PoisonError::into_inner);
        let shadow = match shadows.get(&key) {
            Some(t) => t.clone(),
            None => {
                let t = Texture::new(&self.ctx, sd).map_err(|e| refuse(&format!("{e:?}")))?;
                shadows.insert(key, t.clone());
                t
            }
        };
        Ok(BlockTarget {
            shadow,
            target: res.tex.clone(),
            region: block_region(&d, level, layer),
            row_bytes: sd.width * format.block_bytes(),
        })
    }
}
