//! The block-linear surface of a record as the kernels of `nxkit_metal` read it, with the levels of the guest layout
//! where they differ from the rules of `nxkit_metal`.

use nxkit_metal::{GobOrder, SurfaceLayout};
use nxkit_nvn::format::format_info;
use nxkit_nvn::layout::Target;
use nxkit_nvn::resource::TextureRecord;

use crate::skip::Skip;
use crate::textures::Res;

/// The block-linear layout of a record, in blocks.
pub(super) fn surface(
    rec: &TextureRecord,
    res: &Res,
    order: GobOrder,
) -> Result<SurfaceLayout, Skip> {
    if rec.layout.linear {
        return Err(Skip::Transfer);
    }
    let d = &rec.desc;
    let info = format_info(d.format).map_err(|_| Skip::Format)?;
    let (bw, bh) = (
        u32::from(info.block_w).max(1),
        u32::from(info.block_h).max(1),
    );
    let (width, height, depth, layers) = match d.target {
        Target::D1 => (d.width, 1, 1, 1),
        Target::D1Array => (d.width, 1, 1, d.height.max(1)),
        Target::D3 => (d.width, d.height, d.depth.max(1), 1),
        Target::D2Array | Target::D2MultisampleArray => (d.width, d.height, 1, d.depth.max(1)),
        Target::CubemapArray => (d.width, d.height, 1, d.depth.max(6)),
        Target::Cubemap => (d.width, d.height, 1, 6),
        _ => (d.width, d.height, 1, 1),
    };
    Ok(SurfaceLayout {
        bytes_per_block: u32::from(info.bytes_per_block),
        width: width.div_ceil(bw),
        height: height.div_ceil(bh),
        depth,
        mip_levels: res.desc.mip_levels,
        layers,
        block_height: 1 << u32::from(rec.layout.tile_h).min(5),
        block_depth: 1 << u32::from(rec.layout.tile_d).min(5),
        order,
        levels: None,
    }
    .with_guest_levels(rec))
}

/// The levels of the guest layout where they differ from the rules of `nxkit_metal`: a sparse texture pads its tiled
/// levels to whole 64 KiB tiles and its layers to whole tiles, which the rules do not know. Read with the rules, every
/// row of blocks of a padded level slid sideways by the padding (Octopath Traveler's portraits and its language
/// notice: bands about one tile high, shifted against each other, with pieces of the padding between them).
trait GuestLevels {
    fn with_guest_levels(self, rec: &TextureRecord) -> SurfaceLayout;
}

impl GuestLevels for SurfaceLayout {
    fn with_guest_levels(mut self, rec: &TextureRecord) -> SurfaceLayout {
        let d = &rec.desc;
        let n = self.mip_levels as usize;
        if d.samples > 1 || n > nxkit_metal::MAX_TABLE_LEVELS {
            return self;
        }
        let mut t = nxkit_metal::LevelTable {
            offsets: [0; nxkit_metal::MAX_TABLE_LEVELS],
            gobs: [0; nxkit_metal::MAX_TABLE_LEVELS],
            layer_stride: rec.layout.layer_size,
        };
        for i in 0..n {
            let (Ok(off), Ok(gobs)) = (d.level_offset(i as u32), d.level_gobs(i as u32)) else {
                return self;
            };
            t.offsets[i] = off;
            t.gobs[i] = gobs;
        }
        let same = (0..n).all(|i| {
            self.level(i as u32)
                .is_ok_and(|l| l.offset == t.offsets[i] && l.row_bytes.div_ceil(64) == t.gobs[i])
        }) && (self.layers == 1 || self.layer_stride().ok() == Some(t.layer_stride));
        if !same {
            self.levels = Some(t);
        }
        if crate::diag("upload") && (d.is_sparse() || !same) {
            eprintln!(
                "DIAG upload layout {:?} format {:#x} {}x{}x{} levels {} flags {:#x} packaged {:?} block 2^{}: {}",
                rec.id,
                d.format,
                d.width,
                d.height,
                d.depth,
                d.levels,
                d.flags,
                d.packaged_block,
                rec.layout.tile_h,
                if same {
                    "as the rules".to_string()
                } else {
                    format!("guest levels {:?}", &t.gobs[..n])
                }
            );
        }
        self
    }
}
