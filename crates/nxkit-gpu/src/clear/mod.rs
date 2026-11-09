//! The texel bytes of `nvnCommandBufferClearTexture`, `...ClearTexturei` and `...ClearTextureui`.
//!
//! The clear fills a box of a texture with one colour. The encoder packs that colour into the bytes of one texel of the
//! destination format and blits a block of such texels from the scratch buffer. Every uncompressed colour format of
//! Metal that NVN maps to has a packing; depth, stencil and compressed formats have none (NVN clears those through
//! render targets, not with this call).

#[cfg(test)]
mod tests;

use nxkit_metal::Format;

/// The clear value type of the call: the colour words are `f32` bits, `i32` values or `u32` values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Float,
    Int,
    Uint,
}

impl Kind {
    /// The kind of the record argument: 0 for `ClearTexture`, 1 for `ClearTexturei`, 2 for `ClearTextureui`.
    fn of(kind: u32) -> Option<Self> {
        match kind {
            0 => Some(Self::Float),
            1 => Some(Self::Int),
            2 => Some(Self::Uint),
            _ => None,
        }
    }
}

/// How one channel of an array format is stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Chan {
    /// Unsigned normalised, of 1 or 2 bytes.
    Unorm(usize),
    /// Signed normalised, of 1 or 2 bytes.
    Snorm(usize),
    /// Half float.
    Float16,
    /// Single float.
    Float32,
    /// Unsigned integer, of 1, 2 or 4 bytes.
    Uint(usize),
    /// Signed integer, of 1, 2 or 4 bytes.
    Sint(usize),
}

impl Chan {
    /// The clear value type that the channel takes.
    fn kind(self) -> Kind {
        match self {
            Self::Uint(_) => Kind::Uint,
            Self::Sint(_) => Kind::Int,
            _ => Kind::Float,
        }
    }
}

/// How a format is laid out, for packing one texel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layout {
    /// Channels of one type, one after the other. `swap` puts blue first (BGRA). With `srgb` the colour channels are
    /// sRGB encoded (8 bit unorm only).
    Array {
        chan: Chan,
        channels: usize,
        swap: bool,
        srgb: bool,
    },
    /// One 16 or 32 bit word of fields, listed from the least significant bit: the channel (0 red .. 3 alpha) and the
    /// bit width of each field. With `int` the fields are unsigned integers, else unsigned normalised.
    Packed {
        fields: &'static [(usize, u32)],
        bytes: usize,
        int: bool,
    },
    /// Packed floats: 11 bit red and green, 10 bit blue.
    Rg11B10,
    /// Three 9 bit mantissas with a shared 5 bit exponent.
    Rgb9E5,
}

impl Layout {
    /// The number of channels the format has: the NVN channel mask must cover them.
    fn channels(self) -> usize {
        match self {
            Self::Array { channels, .. } => channels,
            Self::Packed { fields, .. } => fields.len(),
            Self::Rg11B10 | Self::Rgb9E5 => 3,
        }
    }

    fn kind(self) -> Kind {
        match self {
            Self::Array { chan, .. } => chan.kind(),
            Self::Packed { int: true, .. } => Kind::Uint,
            _ => Kind::Float,
        }
    }
}

fn layout(format: Format) -> Option<Layout> {
    use Chan::*;
    use Format::*;
    let a = |chan, channels| Layout::Array {
        chan,
        channels,
        swap: false,
        srgb: false,
    };
    let bgra = |srgb| Layout::Array {
        chan: Unorm(1),
        channels: 4,
        swap: true,
        srgb,
    };
    let packed = |fields, bytes, int| Layout::Packed { fields, bytes, int };
    Some(match format {
        R8Unorm => a(Unorm(1), 1),
        Rg8Unorm => a(Unorm(1), 2),
        Rgba8Unorm => a(Unorm(1), 4),
        Rgba8UnormSrgb => Layout::Array {
            chan: Unorm(1),
            channels: 4,
            swap: false,
            srgb: true,
        },
        Bgra8Unorm => bgra(false),
        Bgra8UnormSrgb => bgra(true),
        R8Snorm => a(Snorm(1), 1),
        Rg8Snorm => a(Snorm(1), 2),
        Rgba8Snorm => a(Snorm(1), 4),
        R16Unorm => a(Unorm(2), 1),
        Rg16Unorm => a(Unorm(2), 2),
        Rgba16Unorm => a(Unorm(2), 4),
        R16Snorm => a(Snorm(2), 1),
        Rg16Snorm => a(Snorm(2), 2),
        Rgba16Snorm => a(Snorm(2), 4),
        R16Float => a(Float16, 1),
        Rg16Float => a(Float16, 2),
        Rgba16Float => a(Float16, 4),
        R32Float => a(Float32, 1),
        Rg32Float => a(Float32, 2),
        Rgba32Float => a(Float32, 4),
        R8Uint => a(Uint(1), 1),
        Rg8Uint => a(Uint(1), 2),
        Rgba8Uint => a(Uint(1), 4),
        R16Uint => a(Uint(2), 1),
        Rg16Uint => a(Uint(2), 2),
        Rgba16Uint => a(Uint(2), 4),
        R32Uint => a(Uint(4), 1),
        Rg32Uint => a(Uint(4), 2),
        Rgba32Uint => a(Uint(4), 4),
        R8Sint => a(Sint(1), 1),
        Rg8Sint => a(Sint(1), 2),
        Rgba8Sint => a(Sint(1), 4),
        R16Sint => a(Sint(2), 1),
        Rg16Sint => a(Sint(2), 2),
        Rgba16Sint => a(Sint(2), 4),
        R32Sint => a(Sint(4), 1),
        Rg32Sint => a(Sint(4), 2),
        Rgba32Sint => a(Sint(4), 4),
        Rgb10A2Unorm => packed(&[(0, 10), (1, 10), (2, 10), (3, 2)], 4, false),
        Rgb10A2Uint => packed(&[(0, 10), (1, 10), (2, 10), (3, 2)], 4, true),
        // Metal names the packed 16 bit formats from the least significant bit.
        B5G6R5Unorm => packed(&[(2, 5), (1, 6), (0, 5)], 2, false),
        Bgr5A1Unorm => packed(&[(2, 5), (1, 5), (0, 5), (3, 1)], 2, false),
        A1Bgr5Unorm => packed(&[(3, 1), (2, 5), (1, 5), (0, 5)], 2, false),
        Abgr4Unorm => packed(&[(3, 4), (2, 4), (1, 4), (0, 4)], 2, false),
        Rg11B10Float => Layout::Rg11B10,
        Rgb9E5Float => Layout::Rgb9E5,
        _ => return None,
    })
}

/// A non-negative float as an unsigned small float with a 5 bit exponent and `mantissa` bits, as in R11G11B10F.
/// Negative values and NaN give 0, values past the largest finite one give the largest finite one.
fn small_float(v: f32, mantissa: u32) -> u32 {
    if v.is_nan() || v <= 0.0 {
        return 0;
    }
    let bits = v.to_bits();
    let exp = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let max = (30 << mantissa) | ((1 << mantissa) - 1);
    if exp >= 31 {
        return max;
    }
    if exp <= 0 {
        // A denormal of the small float: shift the full mantissa down.
        let m = (bits & 0x7f_ffff) | 0x80_0000;
        let shift = 23 - mantissa as i32 + 1 - exp;
        return if shift > 31 { 0 } else { m >> shift };
    }
    ((exp as u32) << mantissa) | ((bits & 0x7f_ffff) >> (23 - mantissa))
}

/// Three floats as RGB9E5, after the shared exponent rule of `EXT_texture_shared_exponent`: red in the low 9 bits,
/// then green and blue, the exponent in the top 5 bits.
fn rgb9e5(c: [f32; 3]) -> u32 {
    const MANTISSA: i32 = 9;
    const BIAS: i32 = 15;
    let max_value = 511.0 / 512.0 * 65536.0;
    let c = c.map(|v| {
        if v.is_nan() {
            0.0
        } else {
            v.clamp(0.0, max_value)
        }
    });
    let largest = c[0].max(c[1]).max(c[2]);
    let floor_log2 = if largest > 0.0 {
        largest.log2().floor() as i32
    } else {
        -BIAS - 1
    };
    let mut exp = floor_log2.max(-BIAS - 1) + 1 + BIAS;
    let scale = |exp: i32| 2f32.powi(exp - BIAS - MANTISSA);
    if (largest / scale(exp) + 0.5).floor() as u32 == 1 << MANTISSA {
        exp += 1;
    }
    let s = scale(exp);
    let m = c.map(|v| ((v / s + 0.5).floor() as u32).min(511));
    m[0] | m[1] << 9 | m[2] << 18 | (exp as u32) << 27
}

/// A linear value as an 8 bit sRGB code.
fn srgb8(v: f32) -> u32 {
    let v = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
    let e = if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    (e * 255.0 + 0.5) as u32
}

/// A float in [0, 1] as an unsigned normalised value of `bits` bits. NaN gives 0.
fn unorm(v: f32, bits: u32) -> u32 {
    let max = ((1u64 << bits) - 1) as f32;
    if v.is_nan() {
        0
    } else {
        (v.clamp(0.0, 1.0) * max + 0.5) as u32
    }
}

/// A float in [-1, 1] as a signed normalised value of `bits` bits, in two's complement. NaN gives 0.
fn snorm(v: f32, bits: u32) -> u32 {
    let max = ((1u32 << (bits - 1)) - 1) as f32;
    let v = if v.is_nan() { 0.0 } else { v.clamp(-1.0, 1.0) };
    ((v * max).round() as i32) as u32
}

/// The IEEE half float of `v`, rounded to nearest even.
pub(crate) fn half(v: f32) -> u16 {
    let bits = v.to_bits();
    let sign = (bits >> 16 & 0x8000) as u16;
    let exp = (bits >> 23 & 0xff) as i32;
    let man = bits & 0x7f_ffff;
    if exp == 0xff {
        return sign | 0x7c00 | if man != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = man | 0x80_0000;
        let shift = (14 - e) as u32;
        let (q, rem, half_way) = (m >> shift, m & ((1 << shift) - 1), 1 << (shift - 1));
        let q = q + u32::from(rem > half_way || (rem == half_way && q & 1 == 1));
        return sign | q as u16;
    }
    let (q, rem) = ((e as u32) << 10 | man >> 13, man & 0x1fff);
    // A carry out of the mantissa raises the exponent, which is the correct rounding up.
    sign | (q + u32::from(rem > 0x1000 || (rem == 0x1000 && q & 1 == 1))) as u16
}

/// One channel as the bytes of `chan`. `word` is the colour word of the channel in the clear value type of `chan`.
fn channel(chan: Chan, word: u32, srgb: bool) -> Vec<u8> {
    let f = f32::from_bits(word);
    let n = |v: u32, bytes: usize| v.to_le_bytes()[..bytes].to_vec();
    match chan {
        Chan::Unorm(1) if srgb => n(srgb8(f), 1),
        Chan::Unorm(b) => n(unorm(f, 8 * b as u32), b),
        Chan::Snorm(b) => n(snorm(f, 8 * b as u32), b),
        Chan::Float16 => half(f).to_le_bytes().to_vec(),
        Chan::Float32 => word.to_le_bytes().to_vec(),
        Chan::Uint(b) => n(word.min((u64::from(u32::MAX) >> (32 - 8 * b)) as u32), b),
        Chan::Sint(b) => {
            let bits = 8 * b as u32;
            let (lo, hi) = (-(1i64 << (bits - 1)), (1i64 << (bits - 1)) - 1);
            n(i64::from(word as i32).clamp(lo, hi) as u32, b)
        }
    }
}

/// One texel of `format` that holds `color`, for a clear of `kind` (0 for `f32` bits, 1 for `i32` values, 2 for `u32`
/// values) with the NVN channel `mask`. Integer values outside the range of a channel are clamped to it. The error
/// says why the clear cannot be done as a fill: the format has no packing, the kind does not suit the format, or the
/// mask leaves a channel of the format alone.
pub(crate) fn texel(
    format: Format,
    kind: u32,
    color: [u32; 4],
    mask: u32,
) -> Result<Vec<u8>, &'static str> {
    let l = layout(format).ok_or("the format has no clear packing")?;
    let needed = (1u32 << l.channels()) - 1;
    if mask & needed != needed {
        return Err("the channel mask leaves a channel of the format unwritten");
    }
    if Kind::of(kind) != Some(l.kind()) {
        return Err("the clear value type does not suit the format");
    }
    let f = |i: usize| f32::from_bits(color[i]);
    Ok(match l {
        Layout::Array {
            chan,
            channels,
            swap,
            srgb,
        } => (0..channels)
            .flat_map(|c| {
                let src = if swap && c < 3 { 2 - c } else { c };
                // Alpha stays linear in an sRGB format.
                channel(chan, color[src], srgb && src < 3)
            })
            .collect(),
        Layout::Packed { fields, bytes, int } => {
            let mut word = 0u32;
            let mut at = 0;
            for &(c, bits) in fields {
                let v = if int {
                    color[c].min((1 << bits) - 1)
                } else {
                    unorm(f(c), bits)
                };
                word |= v << at;
                at += bits;
            }
            word.to_le_bytes()[..bytes].to_vec()
        }
        Layout::Rg11B10 => {
            let w = small_float(f(0), 6) | small_float(f(1), 6) << 11 | small_float(f(2), 5) << 22;
            w.to_le_bytes().to_vec()
        }
        Layout::Rgb9E5 => rgb9e5([f(0), f(1), f(2)]).to_le_bytes().to_vec(),
    })
}
