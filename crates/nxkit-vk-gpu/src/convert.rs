//! The unpacking of the packed formats that Metal does not have (the reference layer's `conv_unpack`): linear images
//! that the CPU writes in `R4G4`, `B4G4R4A4`, `B5G6R5` or `B5G5R5A1` are stored as 8 bits per component.

use nxkit_vk::format::Conversion;

fn x4(v: u32) -> u8 {
    (v * 17) as u8
}

fn x5(v: u32) -> u8 {
    ((v << 3) | (v >> 2)) as u8
}

fn x6(v: u32) -> u8 {
    ((v << 2) | (v >> 4)) as u8
}

/// The bytes of one unpacked texel: 2 for `R4G4` (into `Rg8Unorm`), 4 for the others (into `Rgba8Unorm`).
pub(crate) fn texel_bytes(c: Conversion) -> usize {
    match c {
        Conversion::R4g4 => 2,
        _ => 4,
    }
}

/// Unpacks `h` rows of `w` texels, `pitch` bytes apart in `src`, into tightly packed rows. `src` too short gives the
/// rows it holds.
pub(crate) fn unpack(c: Conversion, src: &[u8], pitch: usize, w: usize, h: usize) -> Vec<u8> {
    let ob = texel_bytes(c);
    let mut out = vec![0u8; w * h * ob];
    for y in 0..h {
        let Some(row) = src.get(y * pitch..) else {
            break;
        };
        for x in 0..w {
            let Some(o) = out.get_mut((y * w + x) * ob..(y * w + x + 1) * ob) else {
                continue;
            };
            if c == Conversion::R4g4 {
                let v = u32::from(row.get(x).copied().unwrap_or(0));
                o[0] = x4(v >> 4);
                o[1] = x4(v & 15);
                continue;
            }
            let lo = row.get(2 * x).copied().unwrap_or(0);
            let hi = row.get(2 * x + 1).copied().unwrap_or(0);
            let v = u32::from(lo) | (u32::from(hi) << 8);
            let px = match c {
                Conversion::B4g4r4a4 => [x4(v >> 4 & 15), x4(v >> 8 & 15), x4(v >> 12), x4(v & 15)],
                Conversion::B5g6r5 => [x5(v & 31), x6(v >> 5 & 63), x5(v >> 11), 255],
                Conversion::B5g5r5a1 => [
                    x5(v >> 1 & 31),
                    x5(v >> 6 & 31),
                    x5(v >> 11),
                    if v & 1 != 0 { 255 } else { 0 },
                ],
                Conversion::None | Conversion::R4g4 => [0; 4],
            };
            o.copy_from_slice(&px);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_texels_widen_to_eight_bits() {
        // R4G4: red in the high nibble.
        assert_eq!(unpack(Conversion::R4g4, &[0xF0], 1, 1, 1), vec![255, 0]);
        // B5G6R5: blue in the low bits, red in the high.
        let v: u16 = 0xF800; // red 31, green 0, blue 0
        assert_eq!(
            unpack(Conversion::B5g6r5, &v.to_le_bytes(), 2, 1, 1),
            vec![0, 0, 255, 255]
        );
        // B5G5R5A1: alpha in bit 0.
        let v: u16 = 0x003F; // blue 0, green 0, red 31, alpha 1
        assert_eq!(
            unpack(Conversion::B5g5r5a1, &v.to_le_bytes(), 2, 1, 1),
            vec![255, 0, 0, 255]
        );
        // B4G4R4A4: alpha in the low nibble, then red, green, blue.
        let v: u16 = 0x1234;
        assert_eq!(
            unpack(Conversion::B4g4r4a4, &v.to_le_bytes(), 2, 1, 1),
            vec![0x33, 0x22, 0x11, 0x44]
        );
    }

    #[test]
    fn rows_follow_the_pitch() {
        let src = [0xF0, 0, 0, 0, 0x0F, 0, 0, 0];
        assert_eq!(
            unpack(Conversion::R4g4, &src, 4, 1, 2),
            vec![255, 0, 0, 255]
        );
        // a short source leaves the missing rows zero
        assert_eq!(
            unpack(Conversion::R4g4, &src[..1], 4, 1, 2),
            vec![255, 0, 0, 0]
        );
    }
}
