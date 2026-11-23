//! The texture header (TIC) and the sampler header (TSC), 32 bytes each, as the texture pool and the sampler pool hold them.
//!
//! The words follow the structure of the Maxwell headers: the address in words 1 and 2, the layout kind in word 2,
//! the block shape in word 3 and the size in words 4 and 5. The bit positions are nxkit's own encoding, not the
//! hardware encoding. The game never reads the headers. The front end writes them when the game registers a texture,
//! so the encoding only has to be lossless. Each field has a round-trip test.
//!
//! ## How the fields were checked (2026-11-07)
//!
//! The engine has no hardware header code. Its texture pool is a table in the host (`dpool` in `hle_nvn.c`), and
//! `nvn_texdesc` in `nvn_tex.h` plus `view_t` and `samp_par` in `nvn_obj.h` hold the same values as the fields here.
//! No deko3d or Mesa header (`cl9097tex.h`, `clb197tex.h`) exists on the development machine. A search with Spotlight and
//! in `~/repos/me` found none. So no bit position is checked against hardware.
//! Every bit position is unverified against hardware, and nothing in nxkit needs it to be.
//!
//! Checked against `dm/gfx/nvn/include/nvn.h` and `nvn_tex.h` of the engine:
//!
//! | Field | Bits | Largest value in the engine | Result |
//! |---|---|---|---|
//! | format, view format | 8 | `NVNformat` 156 | fits |
//! | target, view target | 4 | `NVN_TEXTURE_TARGET_BUFFER` 10 | fits, and the discriminants of [`Target`] equal the NVN values |
//! | swizzle, 4 fields | 3 each | `NVN_TEXTURE_SWIZZLE_A` 5 | fits |
//! | samples (log2) | 3 | a power of two up to 128 | fits, nxkit's limit |
//! | levels, view levels | 6 | 15 (`g_devinfo[0x2C]`) | fits |
//! | base level | 5 | 15 | fits |
//! | stencil (depth-stencil mode of the texture or the view) | 1, word 4 bit 25 | `NVN_TEXTURE_DEPTH_STENCIL_MODE_STENCIL` 1 | fits |
//! | width, height, depth minus 1 | 20 | 16384 (`g_devinfo[0x3D]`) | fits |
//! | width minus 1 of a buffer | 27, the upper 7 in the height field | 2^27 (`g_devinfo[0x3E]`) | fits |
//! | layers, base layer | 16 | 2048 (`g_devinfo[0x2D]`) | fits |
//! | flags | 12 | `NVN_TEXTURE_FLAGS_ZCULL_SUPPORT_STENCIL` 0x400 | fits |
//! | block shape `tile_h`, `tile_d` | 3 each | `sparse_tile_h` in `nvn_tex.c` gives 5 at most | fits |
//! | stride | 24 | 16 MiB pitch | unverified, the limit is nxkit's |
//! | wrap s, t, r | 3 each | `NVN_WRAP_MODE_CLAMP_TO_EDGE` 7 | fits |
//! | min filter | 3 | `NVN_MIN_FILTER_LINEAR_MIPMAP_LINEAR` 5 | fits |
//! | mag filter | 2 | `NVN_MAG_FILTER_LINEAR` 1 | fits |
//! | compare mode | 2 | 1 | fits |
//! | compare func | 4 | `NVN_COMPARE_FUNC_ALWAYS` 8 | fits |
//! | reduction | 2 | `NVN_SAMPLER_REDUCTION_MAX` 2 | fits |
//! | anisotropy | 7, in steps of 0.25 | 16.0 (`g_devinfo[0x1B]`), 64 steps | fits, the value is rounded to 0.25 |
//! | lod min, max, bias, border | raw `f32` | `samp_par` holds `float` | exact |
//!
//! The test `the_fields_do_not_overlap_and_hold_the_largest_values` fills every field with its largest value and reads
//! each back, so no field sits on the bits of another. The sampler header is lossless except for the anisotropy.

use crate::error::{NvnError, Result};
use crate::layout::{Target, TextureDesc};
use crate::space::GpuAddr;

/// The bytes of one header.
pub const HEADER_BYTES: usize = 32;

/// How the texels lie in memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TicLayout {
    /// A texel buffer.
    Buffer,
    /// Rows `stride` bytes apart.
    Pitch {
        /// The row stride in bytes.
        stride: u32,
    },
    /// Block-linear with the given log2 block shape in GOBs.
    BlockLinear {
        /// The log2 block height.
        tile_h: u8,
        /// The log2 block depth.
        tile_d: u8,
    },
}

/// A view of a texture: the part of the mip chain and of the layers that a header shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextureView {
    /// The first level.
    pub base_level: u8,
    /// The number of levels. 0 means all levels from `base_level`.
    pub levels: u8,
    /// The first layer.
    pub base_layer: u16,
    /// The number of layers. 0 means all layers from `base_layer`.
    pub layers: u16,
    /// A format that replaces the format of the texture.
    pub format: Option<i32>,
    /// A target that replaces the target of the texture.
    pub target: Option<Target>,
    /// The channel swizzle: 0 zero, 1 one, 2 R, 3 G, 4 B, 5 A, for each of R, G, B, A.
    pub swizzle: [u8; 4],
    /// `NVN_TEXTURE_DEPTH_STENCIL_MODE_STENCIL` of the texture or the view: a depth-stencil texture is read as its
    /// stencil, not its depth.
    pub stencil: bool,
}

impl TextureView {
    /// The view that shows the whole texture with the identity swizzle.
    pub const fn identity() -> Self {
        Self {
            base_level: 0,
            levels: 0,
            base_layer: 0,
            layers: 0,
            format: None,
            target: None,
            swizzle: [2, 3, 4, 5],
            stencil: false,
        }
    }
}

/// The decoded texture header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tic {
    /// The GPU address of the storage.
    pub address: GpuAddr,
    /// The `NVNformat` value.
    pub format: i32,
    /// The target.
    pub target: Target,
    /// The memory layout.
    pub layout: TicLayout,
    /// The sample count, 1 or more.
    pub samples: u8,
    /// The width in texels.
    pub width: u32,
    /// The height in texels, or the layer count of a 1D array.
    pub height: u32,
    /// The depth in texels, or the layer count of an array.
    pub depth: u32,
    /// The number of levels of the texture.
    pub levels: u8,
    /// The view of the texture.
    pub view: TextureView,
    /// The texture flags.
    pub flags: u16,
}

fn field(value: u32, bits: u32, name: &'static str) -> Result<u32> {
    if value >> bits != 0 {
        return Err(NvnError::FieldRange(name));
    }
    Ok(value)
}

const KIND_BUFFER: u32 = 0;
const KIND_PITCH: u32 = 1;
const KIND_BLOCK_LINEAR: u32 = 3;

impl Tic {
    /// The header of a texture at `address` with the layout that `desc` gives and a view.
    pub fn for_texture(desc: &TextureDesc, address: GpuAddr, view: TextureView) -> Result<Self> {
        let layout = desc.layout()?;
        let kind = if desc.target == Target::Buffer {
            TicLayout::Buffer
        } else if layout.linear {
            let stride = if desc.stride != 0 {
                desc.stride
            } else {
                let info = crate::format::format_info(desc.format)?;
                (u32::from(info.bytes_per_block) * desc.width.div_ceil(info.block_w.into()))
                    .next_multiple_of(layout.align)
            };
            TicLayout::Pitch { stride }
        } else {
            TicLayout::BlockLinear {
                tile_h: layout.tile_h,
                tile_d: layout.tile_d,
            }
        };
        let buffer = kind == TicLayout::Buffer;
        Ok(Self {
            address,
            format: desc.format,
            target: desc.target,
            layout: kind,
            samples: desc.samples.max(1),
            width: desc.width,
            // A buffer is one row, whatever height the builder holds (the layout takes it as 1 too).
            height: if buffer { 1 } else { desc.height },
            depth: if buffer { 1 } else { desc.depth.max(1) },
            levels: u8::try_from(desc.level_count()).map_err(|_| NvnError::FieldRange("levels"))?,
            view,
            flags: u16::try_from(desc.flags).map_err(|_| NvnError::FieldRange("flags"))?,
        })
    }

    /// The eight words of the header.
    pub fn encode(&self) -> Result<[u32; 8]> {
        if !self.samples.is_power_of_two() {
            return Err(NvnError::FieldRange("samples"));
        }
        let log2_samples = field(u32::from(self.samples).trailing_zeros(), 3, "samples")?;
        let swz = self
            .view
            .swizzle
            .iter()
            .enumerate()
            .try_fold(0u32, |a, (i, s)| {
                Ok::<u32, NvnError>(a | field(u32::from(*s), 3, "swizzle")? << (3 * i))
            })?;
        let (kind, tile_h, tile_d, stride) = match self.layout {
            TicLayout::Buffer => (KIND_BUFFER, 0, 0, 0),
            TicLayout::Pitch { stride } => (KIND_PITCH, 0, 0, field(stride, 24, "stride")?),
            TicLayout::BlockLinear { tile_h, tile_d } => (
                KIND_BLOCK_LINEAR,
                field(u32::from(tile_h), 3, "tile_h")?,
                field(u32::from(tile_d), 3, "tile_d")?,
                0,
            ),
        };
        let view_format = match self.view.format {
            Some(f) => field(f as u32, 8, "view format")?,
            None => 0,
        };
        let w0 = field(self.format as u32, 8, "format")?
            | (self.target as u32) << 8
            | swz << 12
            | log2_samples << 24
            | u32::from(self.view.format.is_some()) << 27
            | u32::from(self.view.target.is_some()) << 28
            | kind << 29;
        let w2 = (self.address.get() >> 32) as u32
            | tile_h << 8
            | tile_d << 11
            | self.view.target.map_or(0, |t| t as u32) << 14
            | field(u32::from(self.levels), 6, "levels")? << 18
            | field(u32::from(self.view.levels), 6, "view levels")? << 24;
        // A buffer is one row of up to 2^27 texels: the width takes the height field for its upper bits.
        let (w_lo, w5) = if self.layout == TicLayout::Buffer {
            if self.height != 1 {
                return Err(NvnError::FieldRange("height"));
            }
            let w = field(self.width.wrapping_sub(1), 27, "width")?;
            (w & 0xF_FFFF, w >> 20)
        } else {
            (
                field(self.width.wrapping_sub(1), 20, "width")?,
                field(self.height.wrapping_sub(1), 20, "height")?,
            )
        };
        let w4 = w_lo
            | field(u32::from(self.view.base_level), 5, "base level")? << 20
            | u32::from(self.view.stencil) << 25;
        let w6 = field(self.depth.wrapping_sub(1), 20, "depth")?
            | field(u32::from(self.flags), 12, "flags")? << 20;
        let w7 = u32::from(self.view.base_layer) | u32::from(self.view.layers) << 16;
        Ok([
            w0,
            self.address.get() as u32,
            w2,
            stride | view_format << 24,
            w4,
            w5,
            w6,
            w7,
        ])
    }

    /// The header from its eight words.
    pub fn decode(w: &[u32; 8]) -> Result<Self> {
        let bits = |word: u32, lo: u32, n: u32| (word >> lo) & ((1 << n) - 1);
        let kind = bits(w[0], 29, 2);
        let layout = match kind {
            KIND_BUFFER => TicLayout::Buffer,
            KIND_PITCH => TicLayout::Pitch {
                stride: bits(w[3], 0, 24),
            },
            KIND_BLOCK_LINEAR => TicLayout::BlockLinear {
                tile_h: bits(w[2], 8, 3) as u8,
                tile_d: bits(w[2], 11, 3) as u8,
            },
            _ => return Err(NvnError::BadTexture("header layout kind")),
        };
        let mut swizzle = [0u8; 4];
        for (i, s) in swizzle.iter_mut().enumerate() {
            *s = bits(w[0], 12 + 3 * i as u32, 3) as u8;
        }
        let view_target = if bits(w[0], 28, 1) != 0 {
            Some(Target::from_raw(bits(w[2], 14, 4) as u8)?)
        } else {
            None
        };
        let view_format = (bits(w[0], 27, 1) != 0).then(|| bits(w[3], 24, 8) as i32);
        let address = (u64::from(bits(w[2], 0, 8)) << 32) | u64::from(w[1]);
        Ok(Self {
            address: GpuAddr::new(address)?,
            format: bits(w[0], 0, 8) as i32,
            target: Target::from_raw(bits(w[0], 8, 4) as u8)?,
            layout,
            samples: 1 << bits(w[0], 24, 3),
            width: if layout == TicLayout::Buffer {
                (bits(w[4], 0, 20) | bits(w[5], 0, 7) << 20) + 1
            } else {
                bits(w[4], 0, 20) + 1
            },
            height: if layout == TicLayout::Buffer {
                1
            } else {
                bits(w[5], 0, 20) + 1
            },
            depth: bits(w[6], 0, 20) + 1,
            levels: bits(w[2], 18, 6) as u8,
            view: TextureView {
                base_level: bits(w[4], 20, 5) as u8,
                levels: bits(w[2], 24, 6) as u8,
                base_layer: bits(w[7], 0, 16) as u16,
                layers: bits(w[7], 16, 16) as u16,
                format: view_format,
                target: view_target,
                swizzle,
                stencil: bits(w[4], 25, 1) != 0,
            },
            flags: bits(w[6], 20, 12) as u16,
        })
    }

    /// The header as 32 little-endian bytes.
    pub fn to_bytes(&self) -> Result<[u8; HEADER_BYTES]> {
        Ok(words_to_bytes(&self.encode()?))
    }
}

fn words_to_bytes(w: &[u32; 8]) -> [u8; HEADER_BYTES] {
    let mut out = [0u8; HEADER_BYTES];
    for (i, word) in w.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    out
}

/// The decoded sampler header. The values are the `NVN*` enum values of the sampler builder.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tsc {
    /// The minification filter, below 8.
    pub min_filter: u8,
    /// The magnification filter, below 4.
    pub mag_filter: u8,
    /// The wrap mode of S, below 8.
    pub wrap_s: u8,
    /// The wrap mode of T, below 8.
    pub wrap_t: u8,
    /// The wrap mode of R, below 8.
    pub wrap_r: u8,
    /// The depth compare mode, below 4.
    pub compare_mode: u8,
    /// The depth compare function, below 16.
    pub compare_func: u8,
    /// The reduction mode, below 4.
    pub reduction: u8,
    /// The smallest level of detail.
    pub lod_min: f32,
    /// The largest level of detail.
    pub lod_max: f32,
    /// The bias of the level of detail.
    pub lod_bias: f32,
    /// The maximum anisotropy in steps of 0.25, below 32.
    pub aniso: f32,
    /// The border colour.
    pub border: [f32; 4],
}

impl Tsc {
    /// The eight words of the header. The words 4 to 7 hold the border colour.
    /// The words 1 to 3 hold the level of detail as it is. The anisotropy is rounded to 0.25.
    pub fn encode(&self) -> Result<[u32; 8]> {
        let q = (self.aniso * 4.0).round();
        if !(0.0..128.0).contains(&q) {
            return Err(NvnError::FieldRange("anisotropy"));
        }
        let w0 = field(u32::from(self.wrap_s), 3, "wrap s")?
            | field(u32::from(self.wrap_t), 3, "wrap t")? << 3
            | field(u32::from(self.wrap_r), 3, "wrap r")? << 6
            | field(u32::from(self.min_filter), 3, "min filter")? << 9
            | field(u32::from(self.mag_filter), 2, "mag filter")? << 12
            | field(u32::from(self.compare_mode), 2, "compare mode")? << 14
            | field(u32::from(self.compare_func), 4, "compare func")? << 16
            | field(u32::from(self.reduction), 2, "reduction")? << 20
            | (q as u32) << 25;
        let b = self.border;
        Ok([
            w0,
            self.lod_min.to_bits(),
            self.lod_max.to_bits(),
            self.lod_bias.to_bits(),
            b[0].to_bits(),
            b[1].to_bits(),
            b[2].to_bits(),
            b[3].to_bits(),
        ])
    }

    /// The header from its eight words.
    pub fn decode(w: &[u32; 8]) -> Self {
        let bits = |lo: u32, n: u32| ((w[0] >> lo) & ((1 << n) - 1)) as u8;
        Self {
            wrap_s: bits(0, 3),
            wrap_t: bits(3, 3),
            wrap_r: bits(6, 3),
            min_filter: bits(9, 3),
            mag_filter: bits(12, 2),
            compare_mode: bits(14, 2),
            compare_func: bits(16, 4),
            reduction: bits(20, 2),
            aniso: f32::from(bits(25, 7)) / 4.0,
            lod_min: f32::from_bits(w[1]),
            lod_max: f32::from_bits(w[2]),
            lod_bias: f32::from_bits(w[3]),
            border: [
                f32::from_bits(w[4]),
                f32::from_bits(w[5]),
                f32::from_bits(w[6]),
                f32::from_bits(w[7]),
            ],
        }
    }

    /// The header as 32 little-endian bytes.
    pub fn to_bytes(&self) -> Result<[u8; HEADER_BYTES]> {
        Ok(words_to_bytes(&self.encode()?))
    }
}

#[cfg(test)]
#[path = "tic_tests.rs"]
mod tests;
