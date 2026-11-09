//! The packing of clear colours into texels.

use super::*;

fn f(v: [f32; 4]) -> [u32; 4] {
    v.map(f32::to_bits)
}

#[test]
fn eight_bit_colours_round_clamp_and_swap() {
    let c = f([1.0, 0.5, -1.0, 0.25]);
    assert_eq!(
        texel(Format::Rgba8Unorm, 0, c, 0xf).unwrap(),
        [255, 128, 0, 64]
    );
    assert_eq!(
        texel(Format::Bgra8Unorm, 0, c, 0xf).unwrap(),
        [0, 128, 255, 64]
    );
    assert_eq!(texel(Format::R8Unorm, 0, c, 1).unwrap(), [255]);
    assert_eq!(texel(Format::Rg8Unorm, 0, c, 3).unwrap(), [255, 128]);
    assert_eq!(
        texel(Format::Rgba8Unorm, 0, f([f32::NAN, 2.0, 0.0, 1.0]), 0xf).unwrap(),
        [0, 255, 0, 255]
    );
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
    assert_eq!(
        texel(Format::R32Float, 0, c, 1).unwrap(),
        1.0f32.to_le_bytes()
    );
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
    assert_eq!(
        texel(Format::R32Uint, 2, c, 1).unwrap(),
        300u32.to_le_bytes()
    );
}

#[test]
fn a_partial_mask_or_an_unknown_format_is_refused() {
    let c = f([1.0; 4]);
    assert!(texel(Format::Rgba8Unorm, 0, c, 0x7).is_err());
    assert!(texel(Format::R8Unorm, 0, c, 0x6).is_err());
    assert!(texel(Format::Rg8Unorm, 0, c, 0xf).is_ok());
    let w = u32::from_le_bytes(
        texel(Format::Rgb10A2Unorm, 0, f([1.0, 0.0, 0.5, 1.0]), 0xf)
            .unwrap()
            .try_into()
            .unwrap(),
    );
    assert_eq!(w & 0x3ff, 1023);
    assert_eq!(w >> 30, 3);
    let w = u32::from_le_bytes(
        texel(Format::Rg11B10Float, 0, f([1.0, 0.0, 2.0, 0.0]), 0x7)
            .unwrap()
            .try_into()
            .unwrap(),
    );
    assert_eq!(w & 0x7ff, 15 << 6);
    assert_eq!(w >> 22, 16 << 5);
}

#[test]
fn every_colour_format_that_nvn_maps_to_has_a_packing() {
    for nvn in 0..0x100 {
        let Some(m) = crate::format_map::texture_format(nvn) else {
            continue;
        };
        let f = m.format();
        if f.is_compressed() || f.is_depth() || f.has_stencil() {
            assert!(layout(f).is_none(), "{f:?}");
            continue;
        }
        let l = layout(f).unwrap_or_else(|| panic!("NVN {nvn:#x} {f:?} has no clear packing"));
        let kind = match l.kind() {
            Kind::Float => 0,
            Kind::Int => 1,
            Kind::Uint => 2,
        };
        let t = texel(f, kind, [0; 4], 0xf).unwrap();
        assert_eq!(t.len() as u32, f.block_bytes(), "{f:?}");
    }
}

#[test]
fn sixteen_bit_integers_clamp_to_their_range() {
    assert_eq!(
        texel(Format::Rg16Uint, 2, [0x1234, 0x10000, 0, 0], 0x3).unwrap(),
        [0x34, 0x12, 0xff, 0xff]
    );
    assert_eq!(
        texel(Format::Rgba16Uint, 2, [1, 2, 3, 4], 0xf).unwrap(),
        [1, 0, 2, 0, 3, 0, 4, 0]
    );
    assert_eq!(texel(Format::R16Uint, 2, [7, 0, 0, 0], 1).unwrap(), [7, 0]);
}

#[test]
fn a_clear_of_another_value_type_is_converted() {
    // Floats into an unsigned format: rounded, then clamped to the channel.
    assert_eq!(
        texel(Format::Rg16Uint, 0, f([1.0, 70000.0, 0.0, 0.0]), 0x3).unwrap(),
        [1, 0, 0xff, 0xff]
    );
    assert_eq!(
        texel(Format::Rg16Uint, 0, f([0.0; 4]), 0x3).unwrap(),
        [0, 0, 0, 0]
    );
    // A negative signed value into an unsigned format is 0.
    assert_eq!(
        texel(Format::Rg16Uint, 1, [(-5i32) as u32, 3, 0, 0], 0x3).unwrap(),
        [0, 0, 3, 0]
    );
    // An integer into a float format becomes the float.
    assert_eq!(
        texel(Format::R32Float, 2, [2, 0, 0, 0], 1).unwrap(),
        2.0f32.to_le_bytes()
    );
}

#[test]
fn signed_integers_take_the_int_kind_and_clamp() {
    let c = [(-1i32) as u32, 200, (-200i32) as u32, 5];
    assert_eq!(
        texel(Format::Rgba8Sint, 1, c, 0xf).unwrap(),
        [0xff, 127, 0x80, 5]
    );
    assert_eq!(
        texel(Format::Rg16Sint, 1, [(-40000i32) as u32, 40000, 0, 0], 0x3).unwrap(),
        [0x00, 0x80, 0xff, 0x7f]
    );
    assert_eq!(
        texel(Format::R32Sint, 1, c, 1).unwrap(),
        (-1i32).to_le_bytes()
    );
}

#[test]
fn normalised_sixteen_and_signed_formats_round() {
    let c = f([1.0, -1.0, 0.5, 2.0]);
    assert_eq!(
        texel(Format::Rgba16Unorm, 0, c, 0xf).unwrap(),
        [0xff, 0xff, 0, 0, 0x00, 0x80, 0xff, 0xff]
    );
    assert_eq!(
        texel(Format::Rgba8Snorm, 0, c, 0xf).unwrap(),
        [127, 0x81, 64, 127]
    );
    assert_eq!(
        texel(Format::R16Snorm, 0, c, 1).unwrap(),
        0x7fffu16.to_le_bytes()
    );
}

#[test]
fn packed_sixteen_bit_formats_start_at_the_low_bit() {
    let red = f([1.0, 0.0, 0.0, 0.0]);
    let blue = f([0.0, 0.0, 1.0, 1.0]);
    let word =
        |fmt, c, mask| u16::from_le_bytes(texel(fmt, 0, c, mask).unwrap().try_into().unwrap());
    assert_eq!(word(Format::B5G6R5Unorm, red, 0x7), 0x1f << 11);
    assert_eq!(word(Format::B5G6R5Unorm, blue, 0x7), 0x1f);
    assert_eq!(word(Format::Bgr5A1Unorm, blue, 0xf), 0x1f | 1 << 15);
    assert_eq!(word(Format::A1Bgr5Unorm, blue, 0xf), 1 | 0x1f << 1);
    assert_eq!(word(Format::Abgr4Unorm, red, 0xf), 0xf << 12);
    assert!(texel(Format::B5G6R5Unorm, 0, red, 0x3).is_err());
}

#[test]
fn packed_ten_bit_uint_and_shared_exponent() {
    let w = u32::from_le_bytes(
        texel(Format::Rgb10A2Uint, 2, [5, 2000, 0, 7], 0xf)
            .unwrap()
            .try_into()
            .unwrap(),
    );
    assert_eq!(w & 0x3ff, 5);
    assert_eq!(w >> 10 & 0x3ff, 1023);
    assert_eq!(w >> 30, 3);
    assert_eq!(rgb9e5([0.0; 3]), 0);
    // 1.0 is 256 * 2^(16 - 15 - 9): mantissa 256, exponent 16.
    assert_eq!(rgb9e5([1.0, 0.0, 0.0]), 256 | 16 << 27);
    assert_eq!(
        rgb9e5([1.0, 0.5, 0.25]),
        256 | 128 << 9 | 64 << 18 | 16 << 27
    );
    assert_eq!(rgb9e5([f32::NAN, -1.0, 0.0]), 0);
}
