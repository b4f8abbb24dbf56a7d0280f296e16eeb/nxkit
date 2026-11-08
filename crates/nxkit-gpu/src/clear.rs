//! The texel bytes of `nvnCommandBufferClearTexture`.
//!
//! The clear fills a box of a texture with one colour. The encoder packs that colour into the bytes of one texel of the
//! destination format and blits a block of such texels from the scratch buffer.

use nxkit_metal::Format;

/// How many channels a format has, and how to write one texel of it.
#[derive(Clone, Copy)]
enum Layout {
    /// Unsigned normalised bytes. The count is the channels and `swap` puts blue first.
    Unorm8 { channels: usize, swap: bool, srgb: bool },
    /// Half floats.
    Float16 { channels: usize },
    /// Single floats.
    Float32 { channels: usize },
    /// Unsigned integers of 1, 2 or 4 bytes.
    Uint { channels: usize, bytes: usize },
    /// Packed floats: 11 bit red and green, 10 bit blue.
    Rg11B10,
    /// Packed 10 bit unorm red, green and blue with 2 bit alpha.
    Rgb10A2,
}

fn layout(format: Format) -> Option<Layout> {
    use Format::*;
    let unorm = |channels, swap, srgb| Layout::Unorm8 { channels, swap, srgb };
    Some(match format {
        Rgba8Unorm => unorm(4, false, false),
        Rgba8UnormSrgb => unorm(4, false, true),
        Bgra8Unorm => unorm(4, true, false),
        Bgra8UnormSrgb => unorm(4, true, true),
        R8Unorm => unorm(1, false, false),
        Rg8Unorm => unorm(2, false, false),
        R16Float => Layout::Float16 { channels: 1 },
        Rg16Float => Layout::Float16 { channels: 2 },
        Rgba16Float => Layout::Float16 { channels: 4 },
        R32Float => Layout::Float32 { channels: 1 },
        Rg32Float => Layout::Float32 { channels: 2 },
        Rgba32Float => Layout::Float32 { channels: 4 },
        R8Uint => Layout::Uint { channels: 1, bytes: 1 },
        Rgba8Uint => Layout::Uint { channels: 4, bytes: 1 },
        R32Uint => Layout::Uint { channels: 1, bytes: 4 },
        Rg32Uint => Layout::Uint { channels: 2, bytes: 4 },
        Rgba32Uint => Layout::Uint { channels: 4, bytes: 4 },
        Rg11B10Float => Layout::Rg11B10,
        Rgb10A2Unorm => Layout::Rgb10A2,
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

/// A linear value as an 8 bit sRGB code.
fn srgb8(v: f32) -> u8 {
    let v = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
    let e = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
    (e * 255.0 + 0.5) as u8
}

fn unorm8(v: f32) -> u8 {
    if v.is_nan() { 0 } else { (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8 }
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

/// One texel of `format` that holds `color`, for a clear of `kind` (0 for `f32` bits, 2 for `u32` values) with the
/// NVN channel `mask`. The error says why the clear cannot be done as a fill: the format is not supported, the kind
/// does not suit the format, or the mask leaves a channel of the format alone.
pub(crate) fn texel(format: Format, kind: u32, color: [u32; 4], mask: u32) -> Result<Vec<u8>, &'static str> {
    let l = layout(format).ok_or("the format has no clear packing")?;
    let channels = match l {
        Layout::Unorm8 { channels, .. } | Layout::Float16 { channels } | Layout::Float32 { channels } | Layout::Uint { channels, .. } => channels,
        Layout::Rg11B10 => 3,
        Layout::Rgb10A2 => 4,
    };
    let needed = (1u32 << channels) - 1;
    if mask & needed != needed {
        return Err("the channel mask leaves a channel of the format unwritten");
    }
    let is_uint = matches!(l, Layout::Uint { .. });
    if is_uint != (kind == 2) || !matches!(kind, 0 | 2) {
        return Err("the clear value type does not suit the format");
    }
    let f = |i: usize| f32::from_bits(color[i]);
    let mut out = Vec::with_capacity(16);
    match l {
        Layout::Unorm8 { channels, swap, srgb } => {
            for c in 0..channels {
                let src = if swap && c < 3 { 2 - c } else { c };
                // Alpha stays linear in an sRGB format.
                out.push(if srgb && src < 3 { srgb8(f(src)) } else { unorm8(f(src)) });
            }
        }
        Layout::Float16 { channels } => (0..channels).for_each(|c| out.extend(half(f(c)).to_le_bytes())),
        Layout::Float32 { channels } => (0..channels).for_each(|c| out.extend(color[c].to_le_bytes())),
        Layout::Uint { channels, bytes } => {
            for c in 0..channels {
                let v = color[c].min(if bytes == 1 { 0xff } else { u32::MAX });
                out.extend(&v.to_le_bytes()[..bytes]);
            }
        }
        Layout::Rg11B10 => {
            let w = small_float(f(0), 6) | small_float(f(1), 6) << 11 | small_float(f(2), 5) << 22;
            out.extend(w.to_le_bytes());
        }
        Layout::Rgb10A2 => {
            let q = |v: f32, max: f32| (if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) } * max + 0.5) as u32;
            let w = q(f(0), 1023.0) | q(f(1), 1023.0) << 10 | q(f(2), 1023.0) << 20 | q(f(3), 3.0) << 30;
            out.extend(w.to_le_bytes());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(v: [f32; 4]) -> [u32; 4] {
        v.map(f32::to_bits)
    }

    #[test]
    fn eight_bit_colours_round_clamp_and_swap() {
        let c = f([1.0, 0.5, -1.0, 0.25]);
        assert_eq!(texel(Format::Rgba8Unorm, 0, c, 0xf).unwrap(), [255, 128, 0, 64]);
        assert_eq!(texel(Format::Bgra8Unorm, 0, c, 0xf).unwrap(), [0, 128, 255, 64]);
        assert_eq!(texel(Format::R8Unorm, 0, c, 1).unwrap(), [255]);
        assert_eq!(texel(Format::Rg8Unorm, 0, c, 3).unwrap(), [255, 128]);
        assert_eq!(texel(Format::Rgba8Unorm, 0, f([f32::NAN, 2.0, 0.0, 1.0]), 0xf).unwrap(), [0, 255, 0, 255]);
    }

    #[test]
    fn srgb_encodes_colour_and_keeps_alpha_linear() {
        let t = texel(Format::Rgba8UnormSrgb, 0, f([0.5, 0.0, 1.0, 0.5]), 0xf).unwrap();
        assert_eq!(t, [188, 0, 255, 128]);
    }

    #[test]
    fn float_formats_keep_their_bits_or_convert() {
        let c = f([1.0, -2.0, 0.5, 65504.0]);
        assert_eq!(texel(Format::Rgba32Float, 0, c, 0xf).unwrap().len(), 16);
        assert_eq!(texel(Format::R32Float, 0, c, 1).unwrap(), 1.0f32.to_le_bytes());
        let h = texel(Format::Rgba16Float, 0, c, 0xf).unwrap();
        assert_eq!(h, [0x00, 0x3c, 0x00, 0xc0, 0x00, 0x38, 0xff, 0x7b]);
        assert_eq!(half(f32::INFINITY), 0x7c00);
        assert_eq!(half(1e10), 0x7c00);
        assert_eq!(half(0.0), 0);
        assert_eq!(half(-0.0), 0x8000);
        assert_eq!(half(6.1035156e-5), 0x0400);
        assert_eq!(half(5.9604645e-8), 1);
        assert_eq!(half(1.0 + 1.0 / 2048.0), 0x3c00);
        assert_eq!(half(1.0 + 3.0 / 2048.0), 0x3c02);
        assert_eq!(half(f32::NAN) & 0x7c00, 0x7c00);
    }

    #[test]
    fn integer_formats_take_the_uint_kind_and_clamp() {
        let c = [300, 2, 3, 4];
        assert_eq!(texel(Format::Rgba8Uint, 2, c, 0xf).unwrap(), [255, 2, 3, 4]);
        assert_eq!(texel(Format::R32Uint, 2, c, 1).unwrap(), 300u32.to_le_bytes());
        assert!(texel(Format::R32Uint, 0, c, 1).is_err());
        assert!(texel(Format::Rgba8Unorm, 2, c, 0xf).is_err());
    }

    #[test]
    fn a_partial_mask_or_an_unknown_format_is_refused() {
        let c = f([1.0; 4]);
        assert!(texel(Format::Rgba8Unorm, 0, c, 0x7).is_err());
        assert!(texel(Format::R8Unorm, 0, c, 0x6).is_err());
        assert!(texel(Format::Rg8Unorm, 0, c, 0xf).is_ok());
        let w = u32::from_le_bytes(texel(Format::Rgb10A2Unorm, 0, f([1.0, 0.0, 0.5, 1.0]), 0xf).unwrap().try_into().unwrap());
        assert_eq!(w & 0x3ff, 1023);
        assert_eq!(w >> 30, 3);
        let w = u32::from_le_bytes(texel(Format::Rg11B10Float, 0, f([1.0, 0.0, 2.0, 0.0]), 0x7).unwrap().try_into().unwrap());
        assert_eq!(w & 0x7ff, 15 << 6);
        assert_eq!(w >> 22, 16 << 5);
        assert!(texel(Format::Rgba8Unorm, 1, c, 0xf).is_err());
    }
}
