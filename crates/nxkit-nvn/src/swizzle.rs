//! A CPU reference for block-linear storage: the address of a byte, and the copy between block-linear and linear rows.
//!
//! This is the slow, simple path. It is the reference for tests and the fallback when the GPU cannot read a layout.
//! The GOB order is the one that `nvn_deswizzle.c` uses (`THIRD_PARTY.md`).

use crate::error::{NvnError, Result};
use crate::format::format_info;
use crate::layout::{FLAG_LINEAR, FLAG_LINEAR_RENDER_TARGET, Target, TextureDesc};

/// How the 16-byte pieces of a GOB are ordered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GobOrder {
    /// The Tegra X1 order of the Switch.
    #[default]
    Tegra,
    /// The order of the Switch 2.
    Switch2,
}

/// The byte offset of a texel byte inside its GOB of 64 bytes by 8 rows.
pub fn gob_offset(order: GobOrder, x: u32, y: u32) -> u32 {
    match order {
        GobOrder::Tegra => {
            (x % 64 / 32) * 256 + (y % 8 / 2) * 64 + (x % 32 / 16) * 32 + (y % 2) * 16 + x % 16
        }
        GobOrder::Switch2 => {
            (x % 64 / 32) * 256
                + (y % 8 / 4) * 128
                + (x % 32 / 16) * 64
                + (y % 4 / 2) * 32
                + (y % 2) * 16
                + x % 16
        }
    }
}

/// The geometry of one level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LevelGeometry {
    /// The bytes of one row of blocks.
    pub row_bytes: u32,
    /// The rows of blocks.
    pub rows: u32,
    /// The slices (the depth of a 3D texture, else 1).
    pub slices: u32,
    /// The offset of the level in the block-linear storage.
    pub base: u64,
    /// The log2 block height in GOBs.
    pub tile_h: u32,
    /// The log2 block depth in GOBs.
    pub tile_d: u32,
}

impl LevelGeometry {
    /// The bytes of the level when it is linear without padding.
    pub fn linear_bytes(&self) -> u64 {
        u64::from(self.row_bytes) * u64::from(self.rows) * u64::from(self.slices)
    }
}

/// The geometry of a level of a layer. Linear, buffer and multisampled textures are not block-linear.
pub fn level_geometry(d: &TextureDesc, level: u32, layer: u32) -> Result<LevelGeometry> {
    let multisampled = matches!(d.target, Target::D2Multisample | Target::D2MultisampleArray);
    if multisampled || d.samples > 1 {
        return Err(NvnError::Unsupported("deswizzle of a multisampled texture"));
    }
    if d.target == Target::Buffer || d.flags & (FLAG_LINEAR | FLAG_LINEAR_RENDER_TARGET) != 0 {
        return Err(NvnError::BadTexture("the texture is not block-linear"));
    }
    let info = format_info(d.format)?;
    let layout = d.layout()?;
    if level >= d.level_count() || layer >= layout.layers {
        return Err(NvnError::BadTexture("level or layer out of range"));
    }
    let (w, h, depth) = d.level_extent(level);
    let (tile_h, tile_d) = d.level_tiles(level)?;
    Ok(LevelGeometry {
        row_bytes: w.div_ceil(info.block_w.into()) * u32::from(info.bytes_per_block),
        rows: h.div_ceil(info.block_h.into()),
        slices: if d.target == Target::D3 { depth } else { 1 },
        base: u64::from(layer) * layout.layer_size + d.level_offset(level)?,
        tile_h,
        tile_d,
    })
}

/// The offset of byte `x` of row `y` of slice `z` in the block-linear storage of one level.
/// `row_bytes` is the width of the level as laid out, `rows` its block rows.
#[allow(clippy::too_many_arguments)]
pub fn block_linear_offset(
    order: GobOrder,
    (x, y, z): (u32, u32, u32),
    row_bytes: u32,
    rows: u32,
    tile_h: u32,
    tile_d: u32,
) -> u64 {
    let (bh, bd) = (1u32 << tile_h, 1u32 << tile_d);
    let gobs_x = u64::from(row_bytes.div_ceil(64));
    let blocks_y = u64::from(rows.div_ceil(8 * bh));
    let block = 512u64 * u64::from(bh) * u64::from(bd);
    u64::from(z / bd) * gobs_x * blocks_y * block
        + u64::from(y / (8 * bh)) * gobs_x * block
        + u64::from(x / 64) * block
        + (u64::from(z % bd) * u64::from(bh) + u64::from(y % (8 * bh) / 8)) * 512
        + u64::from(gob_offset(order, x, y))
}

/// Calls `f(blocked, linear)` with the storage offset and the linear offset of every byte of a level.
fn walk_level(
    order: GobOrder,
    g: &LevelGeometry,
    gobs: u32,
    pitch: usize,
    linear_len: usize,
    mut f: impl FnMut(usize, usize) -> Result<()>,
) -> Result<()> {
    if pitch < g.row_bytes as usize {
        return Err(NvnError::BufferTooSmall {
            need: u64::from(g.row_bytes),
            have: pitch as u64,
        });
    }
    let need = (pitch as u64) * u64::from(g.rows) * u64::from(g.slices);
    if (linear_len as u64) < need {
        return Err(NvnError::BufferTooSmall {
            need,
            have: linear_len as u64,
        });
    }
    // The block-linear rows are as wide as the GOB columns of the level.
    let laid_out = (gobs * 64).max(g.row_bytes);
    for z in 0..g.slices {
        for y in 0..g.rows {
            let lin_row = (z * g.rows + y) as usize * pitch;
            for x in 0..g.row_bytes {
                let at = g.base
                    + block_linear_offset(order, (x, y, z), laid_out, g.rows, g.tile_h, g.tile_d);
                let at =
                    usize::try_from(at).map_err(|_| NvnError::OutOfRange { addr: at, len: 1 })?;
                f(at, lin_row + x as usize)?;
            }
        }
    }
    Ok(())
}

fn out_of_range(at: usize) -> NvnError {
    NvnError::OutOfRange {
        addr: at as u64,
        len: 1,
    }
}

/// Reads one level of one layer from block-linear storage into `linear`. Rows are `pitch` bytes apart.
/// `storage` is the whole texture storage.
pub fn deswizzle_level(
    d: &TextureDesc,
    order: GobOrder,
    (level, layer): (u32, u32),
    storage: &[u8],
    linear: &mut [u8],
    pitch: usize,
) -> Result<()> {
    let g = level_geometry(d, level, layer)?;
    let gobs = d.level_gobs(level)?;
    let len = linear.len();
    walk_level(order, &g, gobs, pitch, len, |at, to| {
        linear[to] = *storage.get(at).ok_or_else(|| out_of_range(at))?;
        Ok(())
    })
}

/// Writes one level of one layer from `linear` into block-linear storage. Rows are `pitch` bytes apart.
pub fn swizzle_level(
    d: &TextureDesc,
    order: GobOrder,
    (level, layer): (u32, u32),
    linear: &[u8],
    pitch: usize,
    storage: &mut [u8],
) -> Result<()> {
    let g = level_geometry(d, level, layer)?;
    let gobs = d.level_gobs(level)?;
    walk_level(order, &g, gobs, pitch, linear.len(), |at, from| {
        *storage.get_mut(at).ok_or_else(|| out_of_range(at))? = linear[from];
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r8(w: u32, h: u32) -> TextureDesc {
        TextureDesc {
            target: Target::D2,
            format: 0x01,
            flags: 0,
            samples: 0,
            width: w,
            height: h,
            depth: 1,
            levels: 1,
            stride: 0,
            packaged_block: None,
        }
    }

    #[test]
    fn gob_offsets_of_known_bytes() {
        let t = GobOrder::Tegra;
        assert_eq!(gob_offset(t, 0, 0), 0);
        assert_eq!(gob_offset(t, 15, 0), 15);
        assert_eq!(gob_offset(t, 16, 0), 32);
        assert_eq!(gob_offset(t, 32, 0), 256);
        assert_eq!(gob_offset(t, 0, 1), 16);
        assert_eq!(gob_offset(t, 0, 2), 64);
        assert_eq!(gob_offset(t, 48, 7), 496);
        let s = GobOrder::Switch2;
        assert_eq!(gob_offset(s, 0, 4), 128);
        assert_eq!(gob_offset(s, 16, 0), 64);
        assert_eq!(gob_offset(s, 0, 2), 32);
    }

    #[test]
    fn a_gob_covers_each_byte_once() {
        for order in [GobOrder::Tegra, GobOrder::Switch2] {
            let mut seen = [false; 512];
            for y in 0..8 {
                for x in 0..64 {
                    let o = gob_offset(order, x, y) as usize;
                    assert!(!seen[o]);
                    seen[o] = true;
                }
            }
        }
    }

    #[test]
    fn block_height_decides_which_gob_is_next() {
        // 128 wide, 16 rows: two GOB columns and two GOB rows
        let t = GobOrder::Tegra;
        assert_eq!(block_linear_offset(t, (64, 0, 0), 128, 16, 0, 0), 512);
        assert_eq!(block_linear_offset(t, (0, 8, 0), 128, 16, 0, 0), 1024);
        assert_eq!(block_linear_offset(t, (64, 0, 0), 128, 16, 1, 0), 1024);
        assert_eq!(block_linear_offset(t, (0, 8, 0), 128, 16, 1, 0), 512);
    }

    #[test]
    fn small_r8_texture_deswizzles_to_known_bytes() {
        // 64x8 R8 is one GOB. Byte i of the storage holds the texel at the inverse of gob_offset.
        let d = r8(64, 8);
        let storage: Vec<u8> = (0..512u32).map(|i| (i % 251) as u8).collect();
        let mut lin = vec![0u8; 64 * 8];
        deswizzle_level(&d, GobOrder::Tegra, (0, 0), &storage, &mut lin, 64).unwrap();
        assert_eq!(lin[0], storage[0]);
        assert_eq!(lin[16], storage[32]);
        assert_eq!(lin[32], storage[256]);
        assert_eq!(lin[64], storage[16]);
        assert_eq!(lin[2 * 64], storage[64]);
        assert_eq!(
            lin[7 * 64 + 63],
            storage[gob_offset(GobOrder::Tegra, 63, 7) as usize]
        );
    }

    #[test]
    fn swizzle_and_deswizzle_are_inverse_for_levels_and_layers() {
        let mut d = r8(70, 45);
        d.levels = 4;
        d.target = Target::D2Array;
        d.depth = 2;
        let layout = d.layout().unwrap();
        let mut storage = vec![0u8; layout.size as usize];
        for layer in 0..2 {
            for level in 0..4 {
                let g = level_geometry(&d, level, layer).unwrap();
                let pitch = g.row_bytes as usize + 3;
                let src: Vec<u8> = (0..pitch * g.rows as usize)
                    .map(|i| (i as u32 * 7 + level * 31 + layer * 101) as u8)
                    .collect();
                swizzle_level(
                    &d,
                    GobOrder::Tegra,
                    (level, layer),
                    &src,
                    pitch,
                    &mut storage,
                )
                .unwrap();
                let mut back = vec![0u8; src.len()];
                deswizzle_level(
                    &d,
                    GobOrder::Tegra,
                    (level, layer),
                    &storage,
                    &mut back,
                    pitch,
                )
                .unwrap();
                for y in 0..g.rows as usize {
                    let row = y * pitch..y * pitch + g.row_bytes as usize;
                    assert_eq!(
                        src[row.clone()],
                        back[row],
                        "level {level} layer {layer} row {y}"
                    );
                }
            }
        }
    }

    // The hash comes from `nvn_swizzle` (commit 90e1b34), run on the same input.
    #[test]
    fn storage_is_byte_identical_to_reference() {
        let mut d = r8(70, 45);
        d.levels = 4;
        let mut storage = vec![0u8; d.layout().unwrap().size as usize];
        for level in 0..4u32 {
            let g = level_geometry(&d, level, 0).unwrap();
            let pitch = g.row_bytes as usize + 3;
            let src: Vec<u8> = (0..pitch * g.rows as usize)
                .map(|i| (i as u32 * 7 + level * 31) as u8)
                .collect();
            swizzle_level(&d, GobOrder::Tegra, (level, 0), &src, pitch, &mut storage).unwrap();
        }
        let mut h = 1_469_598_103_934_665_603u64;
        for b in &storage {
            h = (h ^ u64::from(*b)).wrapping_mul(1_099_511_628_211);
        }
        assert_eq!((storage.len(), h), (11_776, 0x51fd_33f0_9d13_a13e));
    }

    #[test]
    fn a_3d_texture_round_trips() {
        let mut d = r8(16, 16);
        d.target = Target::D3;
        d.depth = 9;
        let layout = d.layout().unwrap();
        let mut storage = vec![0u8; layout.size as usize];
        let g = level_geometry(&d, 0, 0).unwrap();
        let src: Vec<u8> = (0..g.linear_bytes()).map(|i| (i * 13 + 5) as u8).collect();
        swizzle_level(&d, GobOrder::Switch2, (0, 0), &src, 16, &mut storage).unwrap();
        let mut back = vec![0u8; src.len()];
        deswizzle_level(&d, GobOrder::Switch2, (0, 0), &storage, &mut back, 16).unwrap();
        assert_eq!(src, back);
    }

    #[test]
    fn errors_are_values() {
        let d = r8(64, 8);
        let mut lin = vec![0u8; 10];
        assert!(deswizzle_level(&d, GobOrder::Tegra, (0, 0), &[0; 512], &mut lin, 64).is_err());
        let mut lin = vec![0u8; 512];
        assert!(deswizzle_level(&d, GobOrder::Tegra, (0, 0), &[0; 100], &mut lin, 64).is_err());
        assert!(deswizzle_level(&d, GobOrder::Tegra, (0, 0), &[0; 512], &mut lin, 10).is_err());
        assert!(level_geometry(&d, 1, 0).is_err());
        let mut lr = r8(64, 8);
        lr.flags = FLAG_LINEAR;
        assert!(level_geometry(&lr, 0, 0).is_err());
    }
}
