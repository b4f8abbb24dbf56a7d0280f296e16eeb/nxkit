//! The layout of a sparse texture (`NVN_TEXTURE_FLAGS_SPARSE`, block-linear), a port of the sparse parts of `nvn_tex.c`.
//!
//! A sparse texture is mapped in 64 KiB tiles: one row of blocks, 128 GOBs / (block height × block depth) wide. A level
//! at least one tile wide, high and deep is laid out with its width padded to whole tiles, so it fills whole tiles. The
//! levels from the first smaller one on form the mip tail, packed as usual from there. The storage is whole tiles and
//! 64 KiB aligned. Without a packaged layout a sparse texture whose level 0 is at least one tile large takes the block
//! shape of the standard 64 KiB tile of its block size instead of the automatic one. Games depend on this: Octopath
//! Traveler sizes the mip tail of its sparse textures from these numbers, and textures that come pre-swizzled in the
//! tile shapes are read at the level offsets the host reports.

use super::{FLAG_LINEAR, FLAG_LINEAR_RENDER_TARGET, FLAG_SPARSE, Shape, Target, TextureDesc};
use super::{blocks, mip, sample_factors};
use crate::error::Result;
use crate::format::FormatInfo;

/// The bytes of one sparse tile, also the alignment of a sparse texture.
pub const SPARSE_TILE: u64 = 0x1_0000;

/// The tiled levels of a sparse texture and the size of its tile in texels (`NVNtextureSparseTileLayout`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SparseTiles {
    /// The levels that fill whole tiles. The levels after them are the mip tail.
    pub levels: u32,
    /// The tile width in texels.
    pub width: u32,
    /// The tile height in texels.
    pub height: u32,
    /// The tile depth in texels.
    pub depth: u32,
}

/// The log2 block height in GOBs of the standard 2D tile: 256×256, 256×128, 128×128, 128×64 and 64×64 blocks for
/// 1, 2, 4, 8 and 16 bytes per block.
fn tile_h_2d(bpb: u32) -> Option<u32> {
    match bpb {
        1 => Some(5),
        2 | 4 => Some(4),
        8 | 16 => Some(3),
        _ => None,
    }
}

/// The log2 block height and depth in GOBs of the standard 3D tile: 64×32×32, 32×32×32, 32×32×16, 32×16×16 and
/// 16×16×16 blocks for 1, 2, 4, 8 and 16 bytes per block.
fn tile_hd_3d(bpb: u32) -> Option<(u32, u32)> {
    match bpb {
        1 | 2 => Some((2, 5)),
        4 => Some((2, 4)),
        8 | 16 => Some((1, 4)),
        _ => None,
    }
}

impl TextureDesc {
    /// Whether the texture is sparse and block-linear: a linear or buffer texture with the flag is laid out as usual.
    pub fn is_sparse(&self) -> bool {
        self.flags & FLAG_SPARSE != 0
            && self.flags & (FLAG_LINEAR | FLAG_LINEAR_RENDER_TARGET) == 0
            && self.target != Target::Buffer
    }

    /// The tiled levels and the tile size. A texture that is not sparse has none.
    pub fn sparse_tiles(&self) -> Result<SparseTiles> {
        if !self.is_sparse() {
            return Ok(SparseTiles::default());
        }
        let t = self.checked()?;
        let l = self.layout()?;
        let s = self.shape();
        let (sx, sy) = sample_factors(self.samples);
        let sh = shape_h(self, &s, t, l.tile_h.into());
        let td = u32::from(l.tile_d);
        let gw = (128u32 >> (sh + td)).max(1);
        let (bw, bh, bpb) = (
            u32::from(t.block_w),
            u32::from(t.block_h),
            u32::from(t.bytes_per_block),
        );
        let mut tiles = SparseTiles {
            levels: 0,
            width: gw * 64 / bpb * bw / sx,
            height: if s.dims >= 2 { (8 << sh) * bh / sy } else { 1 },
            depth: if s.dims >= 3 { 1 << td } else { 1 },
        };
        for n in 0..self.level_count() {
            let lwb = blocks(mip(s.w * sx, n), bw) * bpb;
            let lh = if s.dims >= 2 {
                blocks(mip(s.h * sy, n), bh)
            } else {
                1
            };
            let ld = if s.dims >= 3 { mip(s.z, n) } else { 1 };
            if lwb < gw * 64 || lh < 8 << sh || ld < 1 << td {
                break;
            }
            tiles.levels = n + 1;
        }
        Ok(tiles)
    }
}

/// The block shape of a sparse texture without a packaged layout whose level 0 is at least one tile large, or None.
pub(super) fn tile_of(d: &TextureDesc, s: &Shape, t: FormatInfo) -> Option<(u32, u32)> {
    if !d.is_sparse() || d.packaged_block.is_some() {
        return None;
    }
    let (sx, sy) = sample_factors(d.samples);
    let bpb = u32::from(t.bytes_per_block);
    let wb = blocks(s.w * sx, t.block_w.into()) * bpb;
    let hb = blocks(s.h * sy, t.block_h.into());
    match s.dims {
        2 => tile_h_2d(bpb)
            .filter(|&h| wb >= (128 >> h) * 64 && hb >= 8 << h)
            .map(|h| (h, 0)),
        3 => tile_hd_3d(bpb)
            .filter(|&(h, dd)| wb >= (128 >> (h + dd)) * 64 && hb >= 8 << h && s.z >= 1 << dd),
        _ => None,
    }
}

/// The block height that shapes the 64 KiB tiles: the standard one for the block size of a 2D texture without a
/// packaged layout, else the texture's own.
pub(super) fn shape_h(d: &TextureDesc, s: &Shape, t: FormatInfo, tile_h: u32) -> u32 {
    if s.dims == 2 && d.packaged_block.is_none() {
        tile_h_2d(t.bytes_per_block.into()).unwrap_or(tile_h)
    } else {
        tile_h
    }
}

/// The GOB columns of one level after the padding of the tiled levels. `tiled` says whether the levels before this one
/// were tiled. It turns false at the first level of the mip tail and stays false.
pub(super) struct TilePad {
    /// The GOB columns of one tile.
    pub cols: u32,
    /// The log2 block height that shapes the tile.
    pub sh: u32,
    /// The log2 block depth of the texture.
    pub td: u32,
    /// Still in the tiled levels.
    pub tiled: bool,
}

impl TilePad {
    /// The padding of a sparse texture, or one that pads nothing.
    pub(super) fn new(d: &TextureDesc, s: &Shape, t: FormatInfo, tile: (u32, u32)) -> TilePad {
        let sh = shape_h(d, s, t, tile.0);
        TilePad {
            cols: (128u32 >> (sh + tile.1)).max(1),
            sh,
            td: tile.1,
            tiled: d.is_sparse(),
        }
    }

    /// The GOB columns `wg` of a level of `lwb` bytes per row, `lh` block rows and `ld` slices, padded to whole tiles
    /// while the level is still tiled.
    pub(super) fn columns(&mut self, wg: u32, lwb: u32, lh: u32, ld: u32) -> u32 {
        self.tiled =
            self.tiled && lwb >= self.cols * 64 && lh >= 8 << self.sh && ld >= 1 << self.td;
        if self.tiled {
            wg.div_ceil(self.cols) * self.cols
        } else {
            wg
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tex(format: i32, w: u32, h: u32, levels: u32) -> TextureDesc {
        TextureDesc {
            target: Target::D2,
            format,
            flags: FLAG_SPARSE,
            samples: 0,
            width: w,
            height: h,
            depth: 1,
            levels,
            stride: 0,
            packaged_block: None,
        }
    }

    // NVN_FORMAT_RGBA8 is 0x25: 4 bytes per block, the 128×128-block tile, 16 GOBs high.
    const RGBA8: i32 = 0x25;

    #[test]
    fn a_sparse_texture_fills_whole_tiles_and_is_64k_aligned() {
        let d = tex(RGBA8, 512, 512, 10);
        let l = d.layout().unwrap();
        assert_eq!(l.align, SPARSE_TILE as u32);
        assert_eq!(l.size % SPARSE_TILE, 0);
        assert_eq!(l.tile_h, 4);
    }

    #[test]
    fn the_tiled_levels_end_at_the_first_level_smaller_than_a_tile() {
        let t = tex(RGBA8, 512, 512, 10).sparse_tiles().unwrap();
        // 512, 256 and 128 texels are at least one 128×128 tile, and 64 is the mip tail
        assert_eq!((t.levels, t.width, t.height, t.depth), (3, 128, 128, 1));
        let small = tex(RGBA8, 64, 64, 1).sparse_tiles().unwrap();
        assert_eq!(small.levels, 0);
    }

    #[test]
    fn a_texture_that_is_not_sparse_has_no_tiles() {
        let mut d = tex(RGBA8, 512, 512, 10);
        d.flags = 0;
        assert_eq!(d.sparse_tiles().unwrap(), SparseTiles::default());
        assert_eq!(d.layout().unwrap().align, 512);
    }
}
