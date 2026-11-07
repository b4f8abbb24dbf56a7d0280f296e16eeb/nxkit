//! The storage layout of an NVN texture: block-linear (Tegra X1), linear and buffer.
//!
//! This is a port of `nvn_tex.c` of nxnative, which ports the algorithm of deko3d (`dk_image.cpp`).
//! Both notices are in `THIRD_PARTY.md`. Sparse textures return [`NvnError::Unsupported`].

use crate::error::{NvnError, Result};
use crate::format::{FormatInfo, format_info};

/// `NVN_TEXTURE_FLAGS_LINEAR`.
pub const FLAG_LINEAR: u32 = 0x10;
/// `NVN_TEXTURE_FLAGS_SPARSE`.
pub const FLAG_SPARSE: u32 = 0x20;
/// `NVN_TEXTURE_FLAGS_LINEAR_RENDER_TARGET`.
pub const FLAG_LINEAR_RENDER_TARGET: u32 = 0x40;
/// `NVN_TEXTURE_FLAGS_MINIMAL_LAYOUT`.
pub const FLAG_MINIMAL_LAYOUT: u32 = 0x200;

/// `NVNtextureTarget`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// `NVN_TEXTURE_TARGET_1D`.
    D1 = 0,
    /// `NVN_TEXTURE_TARGET_2D`.
    D2 = 1,
    /// `NVN_TEXTURE_TARGET_3D`.
    D3 = 2,
    /// `NVN_TEXTURE_TARGET_1D_ARRAY`. The layer count is in `height`.
    D1Array = 3,
    /// `NVN_TEXTURE_TARGET_2D_ARRAY`. The layer count is in `depth`.
    D2Array = 4,
    /// `NVN_TEXTURE_TARGET_2D_MULTISAMPLE`.
    D2Multisample = 5,
    /// `NVN_TEXTURE_TARGET_2D_MULTISAMPLE_ARRAY`.
    D2MultisampleArray = 6,
    /// `NVN_TEXTURE_TARGET_RECTANGLE`.
    Rectangle = 7,
    /// `NVN_TEXTURE_TARGET_CUBEMAP`.
    Cubemap = 8,
    /// `NVN_TEXTURE_TARGET_CUBEMAP_ARRAY`. The depth counts layer-faces.
    CubemapArray = 9,
    /// `NVN_TEXTURE_TARGET_BUFFER`.
    Buffer = 10,
}

impl Target {
    /// The target of an `NVNtextureTarget` value.
    pub fn from_raw(raw: u8) -> Result<Self> {
        Ok(match raw {
            0 => Self::D1,
            1 => Self::D2,
            2 => Self::D3,
            3 => Self::D1Array,
            4 => Self::D2Array,
            5 => Self::D2Multisample,
            6 => Self::D2MultisampleArray,
            7 => Self::Rectangle,
            8 => Self::Cubemap,
            9 => Self::CubemapArray,
            10 => Self::Buffer,
            _ => return Err(NvnError::BadTexture("target")),
        })
    }
}

/// What the game gives to `nvnTextureBuilder*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextureDesc {
    /// The target.
    pub target: Target,
    /// The `NVNformat` value.
    pub format: i32,
    /// The `NVNtextureFlags` bits.
    pub flags: u32,
    /// The sample count. 0 and 1 mean one sample.
    pub samples: u8,
    /// The width in texels.
    pub width: u32,
    /// The height in texels, or the layer count of a 1D array.
    pub height: u32,
    /// The depth in texels, or the layer count of an array.
    pub depth: u32,
    /// The number of mip levels. 0 means 1.
    pub levels: u32,
    /// The row stride of a linear texture. 0 means the smallest stride.
    pub stride: u32,
    /// Byte 0 of a packaged texture layout (the block height in the low nibble, the block depth in the high nibble).
    pub packaged_block: Option<u8>,
}

/// The size and the block shape of a texture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextureLayout {
    /// The bytes of the whole storage.
    pub size: u64,
    /// The bytes of one layer.
    pub layer_size: u64,
    /// The alignment of the storage in bytes.
    pub align: u32,
    /// The number of layers.
    pub layers: u32,
    /// The log2 of the block height in GOBs.
    pub tile_h: u8,
    /// The log2 of the block depth in GOBs.
    pub tile_d: u8,
    /// True when the storage is pitch linear (or a buffer) and not block-linear.
    pub linear: bool,
}

fn mip(size: u32, level: u32) -> u32 {
    size.checked_shr(level).unwrap_or(0).max(1)
}

fn blocks(size: u32, block: u32) -> u32 {
    size.div_ceil(block)
}

fn pick_tile(gobs: u32) -> u32 {
    match gobs {
        16.. => 4,
        8.. => 3,
        4.. => 2,
        2.. => 1,
        _ => 0,
    }
}

/// The block height that fits the rows of a level. `unit` is the rows of a block unit (8 for a GOB row, 1 for slices).
fn adjust_tile(mut shift: u32, unit: u32, dim: u32) -> u32 {
    if shift == 0 {
        return 0;
    }
    let mut x = unit << (shift - 1);
    if x >= dim {
        loop {
            shift -= 1;
            if shift == 0 {
                break;
            }
            x >>= 1;
            if x < dim {
                break;
            }
        }
    }
    shift
}

fn sample_factors(samples: u8) -> (u32, u32) {
    match samples {
        2 => (2, 1),
        4 => (2, 2),
        8 => (4, 2),
        _ => (1, 1),
    }
}

/// Dimensions in the order that the walk needs: dims per layer (1, 2 or 3), layered, width, height, depth or layers.
struct Shape {
    dims: u32,
    layered: bool,
    w: u32,
    h: u32,
    z: u32,
}

impl TextureDesc {
    fn shape(&self) -> Shape {
        let (w, mut h, mut z) = (self.width, self.height, self.depth);
        let (dims, layered) = match self.target {
            Target::D1 | Target::Buffer => {
                h = 1;
                z = 1;
                (1, false)
            }
            Target::D1Array => {
                z = self.height;
                h = 1;
                (1, true)
            }
            Target::D3 => (3, false),
            Target::D2Array | Target::D2MultisampleArray | Target::CubemapArray => (2, true),
            Target::Cubemap => {
                z = 6;
                (2, true)
            }
            _ => {
                z = 1;
                (2, false)
            }
        };
        Shape {
            dims,
            layered,
            w,
            h,
            z: z.max(1),
        }
    }

    fn checked(&self) -> Result<FormatInfo> {
        if self.flags & FLAG_SPARSE != 0 && self.target != Target::Buffer {
            return Err(NvnError::Unsupported("sparse texture layout"));
        }
        if self.width == 0 {
            return Err(NvnError::BadTexture("width is 0"));
        }
        if self.width > 1 << 20 || self.height > 1 << 20 || self.depth > 1 << 20 {
            return Err(NvnError::BadTexture("size is above 2^20"));
        }
        if self.levels > 32 {
            return Err(NvnError::BadTexture("more than 32 levels"));
        }
        format_info(self.format)
    }

    /// The number of mip levels, at least 1.
    pub fn level_count(&self) -> u32 {
        self.levels.max(1)
    }

    /// The width, height and depth of one level in texels.
    pub fn level_extent(&self, level: u32) -> (u32, u32, u32) {
        let s = self.shape();
        let d = if s.dims >= 3 { mip(s.z, level) } else { 1 };
        let h = if s.dims >= 2 { mip(s.h, level) } else { 1 };
        (mip(s.w, level), h, d)
    }

    /// The bytes of the colour data of one level of one layer, without padding.
    /// A multisampled texture counts every sample.
    pub fn level_bytes(&self, level: u32) -> Result<u64> {
        let info = self.checked()?;
        let (sx, sy) = sample_factors(self.samples);
        let (w, h, d) = self.level_extent(level);
        let (w, h) = (w * sx, h * sy);
        let row = u64::from(blocks(w, info.block_w.into())) * u64::from(info.bytes_per_block);
        Ok(row * u64::from(blocks(h, info.block_h.into())) * u64::from(d))
    }
}

/// Walks the levels. Returns the byte offset of `level` from the start of its layer and its width in GOBs.
fn level_walk(
    d: &TextureDesc,
    level: u32,
    s: &Shape,
    tile: (u32, u32),
    t: FormatInfo,
) -> (u64, u32) {
    let (sx, sy) = sample_factors(d.samples);
    let (bw, bh, bpb) = (
        u32::from(t.block_w),
        u32::from(t.block_h),
        u32::from(t.bytes_per_block),
    );
    let mut off = 0u64;
    let mut i = 0;
    loop {
        let lw = blocks(mip(s.w * sx, i), bw);
        let lh = if s.dims >= 2 {
            blocks(mip(s.h * sy, i), bh)
        } else {
            1
        };
        let ld = if s.dims >= 3 { mip(s.z, i) } else { 1 };
        let th = adjust_tile(tile.0, 8, lh);
        let td = adjust_tile(tile.1, 1, ld);
        let wg = (lw * bpb).div_ceil(64);
        if i == level {
            return (off, wg);
        }
        let ht = (lh.div_ceil(8) + (1 << th) - 1) >> th;
        let dt = (ld + (1 << td) - 1) >> td;
        off += (u64::from(wg) * u64::from(ht) * u64::from(dt)) << (9 + th + td);
        i += 1;
    }
}

fn layer_bytes(d: &TextureDesc, s: &Shape, tile: (u32, u32), t: FormatInfo) -> u64 {
    let mut ls = level_walk(d, d.level_count(), s, tile, t).0;
    if s.layered {
        let hh = blocks(s.h, t.block_h.into());
        let (mut th, mut td) = tile;
        while th != 0 && hh <= (8 << (th - 1)) {
            th -= 1;
        }
        while td != 0 && s.z <= (1 << (td - 1)) {
            td -= 1;
        }
        let shift = th + td + 9;
        let g = ls >> shift;
        if ls != g << shift {
            ls = (g + 1) << shift;
        }
    }
    ls
}

impl TextureDesc {
    /// The storage size, alignment and block shape.
    pub fn layout(&self) -> Result<TextureLayout> {
        let t = self.checked()?;
        let s = self.shape();
        let bpb = u32::from(t.bytes_per_block);
        let base = TextureLayout {
            size: 0,
            layer_size: 0,
            align: 512,
            layers: if s.layered { s.z } else { 1 },
            tile_h: 0,
            tile_d: 0,
            linear: true,
        };
        if self.target == Target::Buffer {
            let size = u64::from(bpb) * u64::from(self.width);
            let align = if bpb == 12 { 4 } else { bpb };
            return Ok(TextureLayout {
                size,
                layer_size: size,
                align,
                ..base
            });
        }
        if self.flags & (FLAG_LINEAR | FLAG_LINEAR_RENDER_TARGET) != 0 {
            let align = if self.flags & FLAG_LINEAR_RENDER_TARGET != 0 {
                128
            } else {
                32
            };
            let stride = if self.stride != 0 {
                self.stride
            } else {
                (bpb * blocks(s.w, t.block_w.into())).next_multiple_of(align)
            };
            let size = u64::from(stride) * u64::from(s.h);
            return Ok(TextureLayout {
                size,
                layer_size: size,
                align,
                ..base
            });
        }
        let (_, sy) = sample_factors(self.samples);
        let mut tile = (0, 0);
        if let Some(b) = self.packaged_block {
            if s.dims >= 2 {
                tile.0 = u32::from(b & 0xf);
            }
            if s.dims == 3 {
                tile.1 = u32::from(b >> 4);
            }
        } else if s.dims == 2 {
            tile.0 = pick_tile(blocks(s.h * sy, t.block_h.into()) * 3 / 16);
        } else if s.dims == 3 {
            tile.1 = pick_tile(s.z + s.z / 2);
        }
        let mut ls = layer_bytes(self, &s, tile, t);
        // MINIMAL_LAYOUT keeps the automatic block shape only when it is not larger than 1-GOB blocks.
        if self.flags & FLAG_MINIMAL_LAYOUT != 0 && self.packaged_block.is_none() && tile != (0, 0)
        {
            let one = layer_bytes(self, &s, (0, 0), t);
            if one != ls {
                tile = (0, 0);
                ls = one;
            }
        }
        Ok(TextureLayout {
            size: if s.layered { ls * u64::from(s.z) } else { ls },
            layer_size: ls,
            tile_h: tile.0 as u8,
            tile_d: tile.1 as u8,
            linear: false,
            ..base
        })
    }

    /// The byte offset of a mip level from the start of its layer. Linear and buffer textures give 0.
    pub fn level_offset(&self, level: u32) -> Result<u64> {
        let t = self.checked()?;
        let l = self.layout()?;
        if l.linear {
            return Ok(0);
        }
        Ok(level_walk(
            self,
            level,
            &self.shape(),
            (l.tile_h.into(), l.tile_d.into()),
            t,
        )
        .0)
    }

    /// The width of a level in GOBs.
    pub fn level_gobs(&self, level: u32) -> Result<u32> {
        let t = self.checked()?;
        let l = self.layout()?;
        Ok(level_walk(
            self,
            level,
            &self.shape(),
            (l.tile_h.into(), l.tile_d.into()),
            t,
        )
        .1)
    }

    /// The log2 block height and block depth that one level uses.
    /// The block height of a level shrinks with the rows of the level.
    pub fn level_tiles(&self, level: u32) -> Result<(u32, u32)> {
        let t = self.checked()?;
        let l = self.layout()?;
        let s = self.shape();
        let (_, sy) = sample_factors(self.samples);
        let lh = if s.dims >= 2 {
            blocks(mip(s.h * sy, level), t.block_h.into())
        } else {
            1
        };
        let ld = if s.dims >= 3 { mip(s.z, level) } else { 1 };
        Ok((
            adjust_tile(l.tile_h.into(), 8, lh),
            adjust_tile(l.tile_d.into(), 1, ld),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desc(w: u32, h: u32, format: i32) -> TextureDesc {
        TextureDesc {
            target: Target::D2,
            format,
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

    // The expected values come from `nvn_tex_layout` of nxnative (commit 90e1b34), run on the same descriptions.
    #[test]
    fn matches_the_nxnative_layouts() {
        let l = desc(64, 16, 0x25).layout().unwrap();
        assert_eq!((l.tile_h, l.size, l.align), (1, 4096, 512));
        let l = desc(256, 256, 0x25).layout().unwrap();
        assert_eq!((l.tile_h, l.size), (4, 262_144));
        let mut d = desc(64, 64, 0x25);
        d.levels = 7;
        let offsets: Vec<u64> = (0..7).map(|i| d.level_offset(i).unwrap()).collect();
        assert_eq!(offsets, [0, 16384, 20480, 21504, 22016, 22528, 23040]);
        assert_eq!(d.layout().unwrap().size, 23_552);
        assert_eq!((d.level_gobs(0).unwrap(), d.level_gobs(1).unwrap()), (4, 2));
        let mut d = desc(700, 300, 0x41);
        d.levels = 10;
        assert_eq!(d.layout().unwrap().size, 244_224);
        assert_eq!(d.level_offset(3).unwrap(), 237_568);
        assert_eq!(d.level_gobs(0).unwrap(), 22);
    }

    #[test]
    fn matches_the_nxnative_layouts_of_other_targets() {
        let mut d = desc(32, 32, 0x25);
        d.target = Target::D3;
        d.depth = 32;
        d.levels = 3;
        let l = d.layout().unwrap();
        assert_eq!((l.tile_d, l.size), (4, 151_552));
        assert_eq!(d.level_offset(1).unwrap(), 131_072);
        let mut d = desc(128, 128, 0x25);
        d.target = Target::Cubemap;
        d.levels = 8;
        let l = d.layout().unwrap();
        assert_eq!((l.layers, l.layer_size, l.size), (6, 90_112, 540_672));
        let mut d = desc(100, 50, 0x25);
        d.target = Target::D2Multisample;
        d.samples = 4;
        assert_eq!(d.layout().unwrap().size, 106_496);
        let mut d = desc(1000, 1, 0x25);
        d.target = Target::D1;
        d.levels = 5;
        assert_eq!(d.layout().unwrap().size, 62_976);
        let mut d = desc(64, 64, 0x25);
        d.target = Target::D2Array;
        d.depth = 3;
        let l = d.layout().unwrap();
        assert_eq!((l.layers, l.layer_size, l.size), (3, 16_384, 49_152));
    }

    #[test]
    fn linear_texture_uses_the_row_alignment() {
        let mut d = desc(10, 4, 0x25);
        d.flags = FLAG_LINEAR;
        let l = d.layout().unwrap();
        assert_eq!((l.size, l.align, l.linear), (64 * 4, 32, true));
        d.flags = FLAG_LINEAR_RENDER_TARGET;
        d.stride = 128;
        assert_eq!(d.layout().unwrap().size, 128 * 4);
    }

    #[test]
    fn buffer_texture_is_a_plain_array() {
        let mut d = desc(100, 1, 0x25);
        d.target = Target::Buffer;
        let l = d.layout().unwrap();
        assert_eq!((l.size, l.align), (400, 4));
    }

    #[test]
    fn minimal_layout_takes_one_gob_blocks_when_smaller() {
        // 320x320 RGB5A1 (Tears of the Kingdom case of nxnative): 204800 bytes with 1-GOB blocks
        let mut d = desc(320, 320, 0x0D);
        d.flags = FLAG_MINIMAL_LAYOUT;
        assert_eq!(d.layout().unwrap().size, 204_800);
        d.flags = 0;
        assert_eq!(d.layout().unwrap().size, 245_760);
    }

    #[test]
    fn packaged_block_height_wins() {
        let mut d = desc(256, 256, 0x25);
        d.packaged_block = Some(0x01);
        assert_eq!(d.layout().unwrap().tile_h, 1);
    }

    #[test]
    fn sparse_and_bad_input_are_errors() {
        let mut d = desc(64, 64, 0x25);
        d.flags = FLAG_SPARSE;
        assert_eq!(
            d.layout(),
            Err(NvnError::Unsupported("sparse texture layout"))
        );
        assert!(desc(0, 4, 0x25).layout().is_err());
        assert!(desc(4, 4, 0).layout().is_err());
        assert!(Target::from_raw(11).is_err());
    }

    #[test]
    fn level_bytes_count_samples_and_blocks() {
        let mut d = desc(10, 10, 0x41);
        assert_eq!(d.level_bytes(0).unwrap(), 3 * 3 * 8);
        d = desc(8, 8, 0x25);
        d.samples = 4;
        d.target = Target::D2Multisample;
        assert_eq!(d.level_bytes(0).unwrap(), 8 * 8 * 4 * 4);
    }
}
