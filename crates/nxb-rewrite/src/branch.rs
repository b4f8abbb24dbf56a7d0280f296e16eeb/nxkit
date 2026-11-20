//! Small readers that the function matcher and its rules share: the displacement of a direct branch and hex text.

pub(crate) fn signed(n: i64, bits: u32) -> i64 {
    if n & (1 << (bits - 1)) != 0 {
        n - (1 << bits)
    } else {
        n
    }
}

/// `(immediate-bit mask, signed byte displacement)` for direct branches.
pub(crate) fn relative(w: u32) -> Option<(u32, i64)> {
    if w & 0x7c00_0000 == 0x1400_0000 {
        return Some((0x03ff_ffff, signed(i64::from(w & 0x3ff_ffff), 26) * 4));
    }
    if w & 0xff00_0010 == 0x5400_0000 || w & 0x7e00_0000 == 0x3400_0000 {
        return Some((0x00ff_ffe0, signed(i64::from((w >> 5) & 0x7_ffff), 19) * 4));
    }
    if w & 0x7e00_0000 == 0x3600_0000 {
        return Some((0x0007_ffe0, signed(i64::from((w >> 5) & 0x3fff), 14) * 4));
    }
    None
}

/// Hexadecimal text to bytes.
pub(crate) fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap_or(0))
        .collect()
}
