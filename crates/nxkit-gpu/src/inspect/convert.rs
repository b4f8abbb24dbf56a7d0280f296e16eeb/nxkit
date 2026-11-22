//! Texels of the common formats as 8-bit pictures, with the range of the values.
//!
//! A picture clamps the values to 0..1. A float format can hold more, and an analysis of overexposure needs the real
//! range. So the conversion also measures the minimum and the maximum of each channel and counts the values above 1,
//! the NaN values and the infinite values. A depth format becomes a grey picture that is stretched over the range of
//! the picture (the manifest holds the range). The alpha of the picture is always opaque. The manifest holds the range
//! of the alpha channel.

use nxkit_metal::Format;

/// The range of the values of a picture, before the clamp.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Stats {
    /// The channels that the format has (1 to 4).
    pub(crate) channels: u8,
    /// The smallest finite value of each channel. NaN when a channel has no finite value.
    pub(crate) min: [f32; 4],
    /// The largest finite value of each channel.
    pub(crate) max: [f32; 4],
    /// The values that are NaN.
    pub(crate) nan: u64,
    /// The values that are infinite.
    pub(crate) inf: u64,
    /// The values above 1 (colour channels only, not alpha).
    pub(crate) over1: u64,
}

/// The layout of the bytes that go to the `.raw` file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RawLayout {
    /// The bytes of one texel.
    pub(crate) bytes_per_pixel: u32,
    /// What the bytes mean, for example `rgba16f`.
    pub(crate) encoding: &'static str,
}

/// A converted picture.
#[derive(Debug)]
pub(crate) struct Converted {
    /// 8-bit RGBA, row by row from the top, `width * height * 4` bytes.
    pub(crate) rgba: Vec<u8>,
    pub(crate) stats: Stats,
    /// Set for float and depth formats: the bytes are also kept as they are.
    pub(crate) raw: Option<RawLayout>,
    /// How the picture was made from the values.
    pub(crate) note: &'static str,
}

/// How a format reads: the bytes of a texel, the channels, the decoder, the raw layout and the kind of picture.
struct Reader {
    bpp: usize,
    channels: u8,
    read: fn(&[u8]) -> [f32; 4],
    raw: Option<&'static str>,
    depth: bool,
    note: &'static str,
}

const CLAMP: &str = "values clamped to 0..1";
const FLOAT: &str = "linear values clamped to 0..1, no gamma";
const DEPTH: &str = "grey stretched from the minimum to the maximum of the picture";

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[2 * i], b[2 * i + 1]])
}

fn f32_at(b: &[u8], i: usize) -> f32 {
    f32::from_le_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]])
}

fn u32_at(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

fn unorm8(v: u8) -> f32 {
    f32::from(v) / 255.0
}

fn unorm16(v: u16) -> f32 {
    f32::from(v) / 65535.0
}

/// An IEEE half float as `f32`.
pub(crate) fn half(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let (exp, mant) = (i32::from(h >> 10 & 0x1f), f32::from(h & 0x3ff));
    match exp {
        0 => sign * mant * 2f32.powi(-24),
        31 if mant == 0.0 => sign * f32::INFINITY,
        31 => f32::NAN,
        _ => sign * (1.0 + mant / 1024.0) * 2f32.powi(exp - 15),
    }
}

/// An unsigned small float with 5 exponent bits and `mant_bits` mantissa bits (11 and 10 bit floats).
pub(crate) fn small_float(bits: u32, mant_bits: u32) -> f32 {
    let exp = (bits >> mant_bits) as i32 & 0x1f;
    let mant = (bits & ((1 << mant_bits) - 1)) as f32 / (1 << mant_bits) as f32;
    match exp {
        0 => mant * 2f32.powi(-14),
        31 if mant == 0.0 => f32::INFINITY,
        31 => f32::NAN,
        _ => (1.0 + mant) * 2f32.powi(exp - 15),
    }
}

fn reader(f: Format) -> Option<Reader> {
    let r = |bpp, channels, read: fn(&[u8]) -> [f32; 4], raw, depth, note| {
        Some(Reader {
            bpp,
            channels,
            read,
            raw,
            depth,
            note,
        })
    };
    match f {
        Format::Rgba8Unorm | Format::Rgba8UnormSrgb => r(
            4,
            4,
            |b| [unorm8(b[0]), unorm8(b[1]), unorm8(b[2]), unorm8(b[3])],
            None,
            false,
            "bytes as they are",
        ),
        Format::Bgra8Unorm | Format::Bgra8UnormSrgb => r(
            4,
            4,
            |b| [unorm8(b[2]), unorm8(b[1]), unorm8(b[0]), unorm8(b[3])],
            None,
            false,
            "blue and red swapped",
        ),
        Format::R8Unorm => r(1, 1, |b| [unorm8(b[0]), 0.0, 0.0, 1.0], None, false, CLAMP),
        Format::Rg8Unorm => r(
            2,
            2,
            |b| [unorm8(b[0]), unorm8(b[1]), 0.0, 1.0],
            None,
            false,
            CLAMP,
        ),
        Format::R16Unorm => r(
            2,
            1,
            |b| [unorm16(u16_at(b, 0)), 0.0, 0.0, 1.0],
            None,
            false,
            CLAMP,
        ),
        Format::Rg16Unorm => r(
            4,
            2,
            |b| [unorm16(u16_at(b, 0)), unorm16(u16_at(b, 1)), 0.0, 1.0],
            None,
            false,
            CLAMP,
        ),
        Format::Rgba16Unorm => r(
            8,
            4,
            |b| {
                [
                    unorm16(u16_at(b, 0)),
                    unorm16(u16_at(b, 1)),
                    unorm16(u16_at(b, 2)),
                    unorm16(u16_at(b, 3)),
                ]
            },
            None,
            false,
            CLAMP,
        ),
        Format::R16Float => r(
            2,
            1,
            |b| [half(u16_at(b, 0)), 0.0, 0.0, 1.0],
            Some("r16f"),
            false,
            FLOAT,
        ),
        Format::Rg16Float => r(
            4,
            2,
            |b| [half(u16_at(b, 0)), half(u16_at(b, 1)), 0.0, 1.0],
            Some("rg16f"),
            false,
            FLOAT,
        ),
        Format::Rgba16Float => r(
            8,
            4,
            |b| {
                [
                    half(u16_at(b, 0)),
                    half(u16_at(b, 1)),
                    half(u16_at(b, 2)),
                    half(u16_at(b, 3)),
                ]
            },
            Some("rgba16f"),
            false,
            FLOAT,
        ),
        Format::R32Float => r(
            4,
            1,
            |b| [f32_at(b, 0), 0.0, 0.0, 1.0],
            Some("r32f"),
            false,
            FLOAT,
        ),
        Format::Rg32Float => r(
            8,
            2,
            |b| [f32_at(b, 0), f32_at(b, 1), 0.0, 1.0],
            Some("rg32f"),
            false,
            FLOAT,
        ),
        Format::Rgba32Float => r(
            16,
            4,
            |b| [f32_at(b, 0), f32_at(b, 1), f32_at(b, 2), f32_at(b, 3)],
            Some("rgba32f"),
            false,
            FLOAT,
        ),
        Format::Rg11B10Float => r(
            4,
            3,
            |b| {
                let w = u32_at(b);
                [
                    small_float(w & 0x7ff, 6),
                    small_float(w >> 11 & 0x7ff, 6),
                    small_float(w >> 22, 5),
                    1.0,
                ]
            },
            Some("rg11b10f, red in the low 11 bits"),
            false,
            FLOAT,
        ),
        Format::Rgb10A2Unorm => r(
            4,
            4,
            |b| {
                let w = u32_at(b);
                [
                    (w & 0x3ff) as f32 / 1023.0,
                    (w >> 10 & 0x3ff) as f32 / 1023.0,
                    (w >> 20 & 0x3ff) as f32 / 1023.0,
                    (w >> 30) as f32 / 3.0,
                ]
            },
            None,
            false,
            CLAMP,
        ),
        Format::Depth32Float => r(
            4,
            1,
            |b| [f32_at(b, 0), 0.0, 0.0, 1.0],
            Some("d32f"),
            true,
            DEPTH,
        ),
        Format::Depth16Unorm => r(
            2,
            1,
            |b| [unorm16(u16_at(b, 0)), 0.0, 0.0, 1.0],
            Some("d16"),
            true,
            DEPTH,
        ),
        _ => None,
    }
}

/// True when [`convert`] knows the format.
#[cfg(test)]
pub(crate) fn supported(f: Format) -> bool {
    reader(f).is_some()
}

/// Makes the picture of `bytes` (`width * height` texels of `format`, row by row with no padding). The error says why a
/// format or a size is not converted.
pub(crate) fn convert(
    format: Format,
    width: u32,
    height: u32,
    bytes: &[u8],
) -> Result<Converted, String> {
    let rd = reader(format).ok_or_else(|| {
        format!("the format {format:?} has no conversion to a picture (integer, snorm, packed or compressed)")
    })?;
    let count = (width as usize) * (height as usize);
    if count == 0 || bytes.len() != count * rd.bpp {
        return Err(format!(
            "{} bytes do not fit {width}x{height} texels of {format:?}",
            bytes.len()
        ));
    }
    let values: Vec<[f32; 4]> = bytes.chunks_exact(rd.bpp).map(rd.read).collect();
    let n = usize::from(rd.channels);
    let mut st = Stats {
        channels: rd.channels,
        min: [f32::INFINITY; 4],
        max: [f32::NEG_INFINITY; 4],
        nan: 0,
        inf: 0,
        over1: 0,
    };
    for v in &values {
        for (c, x) in v.iter().take(n).enumerate() {
            if x.is_nan() {
                st.nan += 1;
            } else if x.is_infinite() {
                st.inf += 1;
                st.over1 += u64::from(*x > 1.0 && c < 3);
            } else {
                st.min[c] = st.min[c].min(*x);
                st.max[c] = st.max[c].max(*x);
                st.over1 += u64::from(*x > 1.0 && c < 3);
            }
        }
    }
    for c in 0..4 {
        if c >= n || st.min[c] > st.max[c] {
            st.min[c] = f32::NAN;
            st.max[c] = f32::NAN;
        }
    }
    let (lo, hi) = (st.min[0], st.max[0]);
    let stretch = |x: f32| {
        if rd.depth && hi > lo {
            (x - lo) / (hi - lo)
        } else if rd.depth {
            0.0
        } else {
            x
        }
    };
    let byte = |x: f32| {
        if x.is_nan() {
            0
        } else {
            (x.clamp(0.0, 1.0) * 255.0).round() as u8
        }
    };
    let mut rgba = Vec::with_capacity(count * 4);
    for v in &values {
        let rgb = match n {
            1 => [stretch(v[0]); 3],
            2 => [v[0], v[1], 0.0],
            _ => [v[0], v[1], v[2]],
        };
        rgba.extend(rgb.map(byte));
        rgba.push(255);
    }
    Ok(Converted {
        rgba,
        stats: st,
        raw: rd.raw.map(|encoding| RawLayout {
            bytes_per_pixel: rd.bpp as u32,
            encoding,
        }),
        note: rd.note,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(v: f32) -> [u8; 2] {
        // Exact for the values of the tests: 0, 0.5, 1, 2, 4.
        let bits: u16 = match v {
            0.0 => 0,
            0.5 => 0x3800,
            1.0 => 0x3c00,
            2.0 => 0x4000,
            4.0 => 0x4400,
            _ => panic!("no half for {v}"),
        };
        bits.to_le_bytes()
    }

    #[test]
    fn half_floats_decode() {
        assert_eq!(half(0x3c00), 1.0);
        assert_eq!(half(0xc000), -2.0);
        assert_eq!(half(0x0001), 2f32.powi(-24));
        assert_eq!(half(0x7c00), f32::INFINITY);
        assert!(half(0x7e00).is_nan());
        assert_eq!(half(0x7bff), 65504.0);
    }

    #[test]
    fn small_floats_decode() {
        // 11 bit: exponent 15 and mantissa 0 is 1. 10 bit: the same.
        assert_eq!(small_float(15 << 6, 6), 1.0);
        assert_eq!(small_float(15 << 5, 5), 1.0);
        assert_eq!(small_float(16 << 6 | 32, 6), 3.0);
        assert_eq!(small_float(0x7c0, 6), f32::INFINITY);
        assert!(small_float(0x7c1, 6).is_nan());
    }

    #[test]
    fn bgra_swaps_red_and_blue() {
        let c = convert(Format::Bgra8Unorm, 2, 1, &[1, 2, 3, 4, 10, 20, 30, 40]).unwrap();
        assert_eq!(c.rgba, [3, 2, 1, 255, 30, 20, 10, 255]);
        assert!(c.raw.is_none());
        let c = convert(Format::Rgba8UnormSrgb, 1, 1, &[1, 2, 3, 4]).unwrap();
        assert_eq!(c.rgba, [1, 2, 3, 255]);
        assert_eq!(c.stats.max[3], 4.0 / 255.0);
    }

    #[test]
    fn rgba16f_clamps_and_keeps_the_range() {
        let mut b = Vec::new();
        for v in [4.0, 0.5, 0.0, 1.0, 2.0, 1.0, 0.0, 0.0] {
            b.extend_from_slice(&h(v));
        }
        let c = convert(Format::Rgba16Float, 2, 1, &b).unwrap();
        assert_eq!(c.rgba, [255, 128, 0, 255, 255, 255, 0, 255]);
        assert_eq!(c.stats.max, [4.0, 1.0, 0.0, 1.0]);
        assert_eq!(c.stats.min, [2.0, 0.5, 0.0, 0.0]);
        // The values above 1: red 4 and red 2 (green 1 is not above 1).
        assert_eq!(c.stats.over1, 2);
        assert_eq!(c.stats.nan + c.stats.inf, 0);
        let raw = c.raw.unwrap();
        assert_eq!((raw.bytes_per_pixel, raw.encoding), (8, "rgba16f"));
    }

    #[test]
    fn nan_and_infinity_are_counted_and_do_not_set_the_range() {
        let mut b = Vec::new();
        for v in [f32::NAN, f32::INFINITY, 0.25] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        let c = convert(Format::R32Float, 3, 1, &b).unwrap();
        assert_eq!(c.rgba, [0, 0, 0, 255, 255, 255, 255, 255, 64, 64, 64, 255]);
        assert_eq!((c.stats.nan, c.stats.inf, c.stats.over1), (1, 1, 1));
        assert_eq!((c.stats.min[0], c.stats.max[0]), (0.25, 0.25));
        assert!(c.stats.min[1].is_nan());
    }

    #[test]
    fn rg11b10f_decodes_each_channel() {
        // Red 1.0 (exponent 15), green 2.0 (exponent 16), blue 0.
        let w: u32 = 15 << 6 | (16 << 6) << 11;
        let c = convert(Format::Rg11B10Float, 1, 1, &w.to_le_bytes()).unwrap();
        assert_eq!(c.rgba, [255, 255, 0, 255]);
        assert_eq!(c.stats.max[..3], [1.0, 2.0, 0.0]);
        assert_eq!(c.stats.channels, 3);
        assert!(c.raw.is_some());
    }

    #[test]
    fn rgb10a2_unpacks() {
        let w: u32 = 1023 | 512 << 10 | 3 << 30;
        let c = convert(Format::Rgb10A2Unorm, 1, 1, &w.to_le_bytes()).unwrap();
        assert_eq!(c.rgba, [255, 128, 0, 255]);
        assert_eq!(c.stats.max[3], 1.0);
    }

    #[test]
    fn sixteen_bit_unorm_and_two_channels() {
        let c = convert(Format::R16Unorm, 2, 1, &[0, 0, 0xff, 0xff]).unwrap();
        assert_eq!(c.rgba, [0, 0, 0, 255, 255, 255, 255, 255]);
        let c = convert(Format::Rg16Unorm, 1, 1, &[0xff, 0xff, 0, 0]).unwrap();
        assert_eq!(c.rgba, [255, 0, 0, 255]);
        let c = convert(Format::Rg16Float, 1, 1, &[0, 0x3c, 0, 0x38]).unwrap();
        assert_eq!(c.rgba, [255, 128, 0, 255]);
    }

    #[test]
    fn depth_is_stretched_to_its_range() {
        let mut b = Vec::new();
        for v in [0.5f32, 0.75, 1.0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        let c = convert(Format::Depth32Float, 3, 1, &b).unwrap();
        assert_eq!(
            c.rgba,
            [0, 0, 0, 255, 128, 128, 128, 255, 255, 255, 255, 255]
        );
        assert_eq!((c.stats.min[0], c.stats.max[0]), (0.5, 1.0));
        assert_eq!(c.raw.unwrap().encoding, "d32f");
        // A flat depth stays black and does not divide by zero.
        let flat = convert(Format::Depth32Float, 1, 1, &1.0f32.to_le_bytes()).unwrap();
        assert_eq!(flat.rgba, [0, 0, 0, 255]);
        let d16 = convert(Format::Depth16Unorm, 2, 1, &[0, 0, 0xff, 0xff]).unwrap();
        assert_eq!(d16.rgba, [0, 0, 0, 255, 255, 255, 255, 255]);
    }

    #[test]
    fn unsupported_formats_and_sizes_say_why() {
        for f in [
            Format::Rgba8Uint,
            Format::Rgba8Snorm,
            Format::R32Uint,
            Format::Stencil8,
            Format::Rgb9E5Float,
        ] {
            assert!(!supported(f));
            assert!(
                convert(f, 1, 1, &[0; 16])
                    .unwrap_err()
                    .contains("no conversion")
            );
        }
        let e = convert(Format::Rgba8Unorm, 2, 2, &[0; 4]).unwrap_err();
        assert!(e.contains("do not fit"), "{e}");
        assert!(convert(Format::Rgba8Unorm, 0, 2, &[]).is_err());
    }
}
