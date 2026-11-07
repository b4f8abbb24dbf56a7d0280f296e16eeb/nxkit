//! The block-linear layout of a surface: the size and offset of each mip level and layer, and the kernel parameters.
//!
//! The layout follows deko3d (`dk_image.cpp`, devkitPro, ISC licence), through its port in `nvn_tex.c` of nxnative.
//! `nxkit-nvn` has a second port in `layout.rs`, which this module does not depend on and is tested against.
//! The rules are:
//!
//! - A level is `max(1, size >> level)` wide, high and deep, measured in blocks.
//! - The block height (in GOBs) of a level starts at the one of the surface and halves while a block would be
//!   at least as tall as the level (`8 * 2^(h - 1) >= rows`). The block depth shrinks the same way against the
//!   depth of the level, with 1 slice per unit.
//! - A level takes `gobs_x * blocks_y * blocks_z` blocks of `512 * block_height * block_depth` bytes.
//! - The levels follow each other. The layers of an array follow each other too, and the size of a layer is
//!   rounded up to a multiple of one block of level 0.

use crate::error::{MetalError, Result};
use crate::format::Format;

/// The order of the 16 byte pieces inside a GOB.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GobOrder {
    /// The Tegra X1 order of the Switch.
    #[default]
    Tegra,
    /// The order of the Switch 2.
    Switch2,
}

/// The whole block-linear storage of a texture: all levels of all layers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SurfaceLayout {
    /// The bytes of one block: 1, 2, 4, 8 or 16.
    pub bytes_per_block: u32,
    /// The width of level 0 in blocks.
    pub width: u32,
    /// The height of level 0 in blocks.
    pub height: u32,
    /// The depth of level 0 in blocks. It is 1 unless the texture is 3D.
    pub depth: u32,
    /// The number of mip levels.
    pub mip_levels: u32,
    /// The number of array layers (6 per cube).
    pub layers: u32,
    /// The block height of the surface in GOBs: 1, 2, 4, 8, 16 or 32. It is not the log2.
    pub block_height: u32,
    /// The block depth of the surface in GOBs: 1, 2, 4, 8, 16 or 32.
    pub block_depth: u32,
    /// The GOB order.
    pub order: GobOrder,
}

/// One level of a layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LevelLayout {
    /// The width in blocks.
    pub width: u32,
    /// The height in blocks.
    pub height: u32,
    /// The depth in blocks.
    pub depth: u32,
    /// The bytes of one row of blocks, without padding.
    pub row_bytes: u32,
    /// The block height in GOBs after the shrink.
    pub block_height: u32,
    /// The block depth in GOBs after the shrink.
    pub block_depth: u32,
    /// The offset from the start of the layer.
    pub offset: u64,
    /// The bytes of the level, with the padding of whole GOBs and blocks.
    pub bytes: u64,
}

fn check_gobs(v: u32, what: &'static str) -> Result<()> {
    if (1..=32).contains(&v) && v.is_power_of_two() {
        Ok(())
    } else {
        Err(MetalError::Invalid(what))
    }
}

/// The block size in GOBs that fits `dim` rows. `unit` is the rows of one GOB row (8) or one slice (1).
fn shrink(gobs: u32, unit: u32, dim: u32) -> u32 {
    let mut g = gobs;
    while g > 1 && unit * (g / 2) >= dim {
        g /= 2;
    }
    g
}

impl SurfaceLayout {
    /// A surface of one level and one layer.
    pub fn single(
        bytes_per_block: u32,
        width: u32,
        height: u32,
        block_height: u32,
        order: GobOrder,
    ) -> Self {
        Self {
            bytes_per_block,
            width,
            height,
            depth: 1,
            mip_levels: 1,
            layers: 1,
            block_height,
            block_depth: 1,
            order,
        }
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if Format::uint_for_block_bytes(self.bytes_per_block).is_none() {
            return Err(MetalError::Invalid(
                "bytes per block must be 1, 2, 4, 8 or 16",
            ));
        }
        if self.width == 0 || self.height == 0 || self.depth == 0 || self.layers == 0 {
            return Err(MetalError::Invalid("the surface is empty"));
        }
        if self.mip_levels == 0
            || self.mip_levels > 32 - self.width.max(self.height).max(self.depth).leading_zeros()
        {
            return Err(MetalError::Invalid(
                "the surface has more mip levels than its size allows",
            ));
        }
        check_gobs(
            self.block_height,
            "block height must be 1, 2, 4, 8, 16 or 32 GOBs",
        )?;
        check_gobs(
            self.block_depth,
            "block depth must be 1, 2, 4, 8, 16 or 32 GOBs",
        )
    }

    fn walk(&self, level: u32) -> LevelLayout {
        let mut offset = 0u64;
        let mut i = 0;
        loop {
            let m = |v: u32| v.checked_shr(i).unwrap_or(0).max(1);
            let (width, height, depth) = (m(self.width), m(self.height), m(self.depth));
            let bh = shrink(self.block_height, 8, height);
            let bd = shrink(self.block_depth, 1, depth);
            let row_bytes = width * self.bytes_per_block;
            let gobs_x = u64::from(row_bytes.div_ceil(64));
            let blocks_y = u64::from(height.div_ceil(8 * bh));
            let blocks_z = u64::from(depth.div_ceil(bd));
            let bytes = gobs_x * blocks_y * blocks_z * 512 * u64::from(bh) * u64::from(bd);
            if i == level {
                return LevelLayout {
                    width,
                    height,
                    depth,
                    row_bytes,
                    block_height: bh,
                    block_depth: bd,
                    offset,
                    bytes,
                };
            }
            offset += bytes;
            i += 1;
        }
    }

    /// The layout of one level.
    pub fn level(&self, level: u32) -> Result<LevelLayout> {
        self.validate()?;
        if level >= self.mip_levels {
            return Err(MetalError::Invalid("the level is outside the surface"));
        }
        Ok(self.walk(level))
    }

    /// The bytes from one layer to the next.
    pub fn layer_stride(&self) -> Result<u64> {
        self.validate()?;
        let last = self.walk(self.mip_levels - 1);
        let total = last.offset + last.bytes;
        if self.layers == 1 {
            return Ok(total);
        }
        let first = self.walk(0);
        let unit = 512 * u64::from(first.block_height) * u64::from(first.block_depth);
        Ok(total.div_ceil(unit) * unit)
    }

    /// The bytes of all layers.
    pub fn total_bytes(&self) -> Result<u64> {
        Ok(self.layer_stride()? * u64::from(self.layers))
    }
}

/// The parameters of one kernel dispatch. The region is inside the level and is measured in blocks.
#[derive(Clone, Copy, Debug)]
pub(crate) struct KernelParams {
    pub base: u64,
    pub level: LevelLayout,
    pub order: GobOrder,
    pub bytes_per_block: u32,
    pub origin: [u32; 3],
    pub extent: [u32; 3],
}

impl KernelParams {
    pub(crate) fn to_bytes(self) -> [u8; 56] {
        let mut b = [0u8; 56];
        b[..8].copy_from_slice(&self.base.to_le_bytes());
        let l = &self.level;
        let words = [
            l.height,
            l.row_bytes,
            self.bytes_per_block,
            l.block_height,
            l.block_depth,
            u32::from(self.order == GobOrder::Switch2),
            self.origin[0],
            self.origin[1],
            self.origin[2],
            self.extent[0],
            self.extent[1],
            self.extent[2],
        ];
        for (i, w) in words.iter().enumerate() {
            b[8 + i * 4..12 + i * 4].copy_from_slice(&w.to_le_bytes());
        }
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn surface(w: u32, h: u32, d: u32, mips: u32, layers: u32, bh: u32, bd: u32) -> SurfaceLayout {
        SurfaceLayout {
            bytes_per_block: 4,
            width: w,
            height: h,
            depth: d,
            mip_levels: mips,
            layers,
            block_height: bh,
            block_depth: bd,
            order: GobOrder::Tegra,
        }
    }

    #[test]
    fn block_height_shrinks_with_the_rows_of_the_level() {
        let s = surface(64, 64, 1, 7, 1, 8, 1);
        let heights: Vec<u32> = (0..7).map(|l| s.level(l).unwrap().block_height).collect();
        // A block shrinks until half of it is smaller than the level: 64 rows keep 8 GOBs, 32 rows take 4.
        assert_eq!(heights, [8, 4, 2, 1, 1, 1, 1]);
    }

    #[test]
    fn levels_follow_each_other() {
        let s = surface(64, 64, 1, 3, 1, 2, 1);
        let (a, b) = (s.level(0).unwrap(), s.level(1).unwrap());
        assert_eq!(a.offset, 0);
        assert_eq!(b.offset, a.bytes);
        // Level 0: 4 GOBs wide, 8 GOB rows high, in blocks of 2 GOBs.
        assert_eq!(a.bytes, 4 * 4 * 1024);
    }

    #[test]
    fn layers_are_rounded_to_one_block_of_level_zero() {
        let s = surface(16, 16, 1, 5, 4, 2, 1);
        let stride = s.layer_stride().unwrap();
        assert_eq!(stride % (512 * 2), 0);
        assert_eq!(s.total_bytes().unwrap(), stride * 4);
    }

    #[test]
    fn depth_shrinks_the_block_depth() {
        let s = surface(32, 32, 8, 4, 1, 1, 4);
        let depths: Vec<u32> = (0..4).map(|l| s.level(l).unwrap().block_depth).collect();
        assert_eq!(depths, [4, 4, 2, 1]);
    }

    #[test]
    fn bad_levels_and_sizes_are_errors() {
        assert!(surface(64, 64, 1, 8, 1, 1, 1).validate().is_err());
        assert!(surface(64, 64, 1, 1, 1, 3, 1).validate().is_err());
        assert!(surface(64, 64, 1, 2, 1, 1, 1).level(2).is_err());
    }
}
