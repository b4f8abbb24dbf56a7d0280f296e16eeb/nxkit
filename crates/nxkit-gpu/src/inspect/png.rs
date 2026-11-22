//! A PNG encoder with the standard library only.
//!
//! The deflate stream holds stored (uncompressed) blocks, so the encoder needs no compression code. A file is a little
//! larger than the raw pixels. Every viewer reads it. The writer makes 8-bit RGBA pictures (colour type 6).

/// The first 8 bytes of every PNG file.
const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// The largest payload of one stored deflate block.
const BLOCK: usize = 65_535;

/// The CRC-32 of the PNG standard (polynomial 0xedb88320) over `parts` taken as one byte string.
pub(crate) fn crc32(parts: &[&[u8]]) -> u32 {
    let mut crc = !0u32;
    for byte in parts.iter().flat_map(|p| p.iter()) {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                crc >> 1 ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// The Adler-32 checksum of `data` (the end of a zlib stream).
pub(crate) fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let (mut a, mut b) = (1u32, 0u32);
    // 5552 bytes is the longest run before `b` can overflow a u32.
    for run in data.chunks(5552) {
        for byte in run {
            a += u32::from(*byte);
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    b << 16 | a
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
}

/// The zlib stream of `raw` in stored blocks.
fn zlib_stored(raw: &[u8]) -> Vec<u8> {
    let mut z = Vec::with_capacity(raw.len() + raw.len() / BLOCK * 5 + 16);
    // Deflate with a 32 KiB window, no dictionary, fastest level. The header is a multiple of 31.
    z.extend_from_slice(&[0x78, 0x01]);
    let mut blocks = raw.chunks(BLOCK).peekable();
    if blocks.peek().is_none() {
        z.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    }
    while let Some(block) = blocks.next() {
        let last = blocks.peek().is_none();
        let len = block.len() as u16;
        z.push(u8::from(last));
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(block);
    }
    z.extend_from_slice(&adler32(raw).to_be_bytes());
    z
}

/// The PNG file of an 8-bit RGBA picture. `rgba` holds `width * height * 4` bytes, row by row from the top. None when
/// the size does not match the bytes or a side is 0.
pub(crate) fn encode_rgba(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let row = (width as usize).checked_mul(4)?;
    if width == 0 || height == 0 || rgba.len() != row.checked_mul(height as usize)? {
        return None;
    }
    // Each row starts with the filter type 0 (none).
    let mut raw = Vec::with_capacity(rgba.len() + height as usize);
    for line in rgba.chunks_exact(row) {
        raw.push(0);
        raw.extend_from_slice(line);
    }
    let z = zlib_stored(&raw);
    let mut out = Vec::with_capacity(z.len() + 64);
    out.extend_from_slice(&SIGNATURE);
    let mut head = Vec::with_capacity(13);
    head.extend_from_slice(&width.to_be_bytes());
    head.extend_from_slice(&height.to_be_bytes());
    // Bit depth 8, colour type 6 (RGBA), deflate, adaptive filtering, no interlace.
    head.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &head);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The chunks of a PNG file as (type, data), with the checksum of each one checked.
    fn chunks(file: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
        assert_eq!(file[..8], SIGNATURE);
        let mut at = 8;
        let mut out = Vec::new();
        while at < file.len() {
            let len = u32::from_be_bytes(file[at..at + 4].try_into().unwrap()) as usize;
            let kind: [u8; 4] = file[at + 4..at + 8].try_into().unwrap();
            let data = file[at + 8..at + 8 + len].to_vec();
            let crc = u32::from_be_bytes(file[at + 8 + len..at + 12 + len].try_into().unwrap());
            assert_eq!(crc, crc32(&[&kind, &data]), "crc of {kind:?}");
            out.push((kind, data));
            at += 12 + len;
        }
        out
    }

    /// Inflates a zlib stream of stored blocks and checks its header and its Adler-32.
    fn unzlib(z: &[u8]) -> Vec<u8> {
        assert_eq!(z[0] & 0x0f, 8, "deflate");
        assert_eq!((u32::from(z[0]) << 8 | u32::from(z[1])) % 31, 0);
        let (mut at, mut out) = (2, Vec::new());
        loop {
            let last = z[at] & 1 == 1;
            assert_eq!(z[at] >> 1, 0, "stored block");
            let len = usize::from(u16::from_le_bytes([z[at + 1], z[at + 2]]));
            let inv = u16::from_le_bytes([z[at + 3], z[at + 4]]);
            assert_eq!(inv, !(len as u16));
            out.extend_from_slice(&z[at + 5..at + 5 + len]);
            at += 5 + len;
            if last {
                break;
            }
        }
        assert_eq!(z[at..], adler32(&out).to_be_bytes());
        out
    }

    #[test]
    fn crc32_matches_the_check_value() {
        assert_eq!(crc32(&[b"123456789"]), 0xcbf4_3926);
        assert_eq!(crc32(&[b"1234", b"56789"]), 0xcbf4_3926);
        assert_eq!(crc32(&[b"IEND"]), 0xae42_6082);
        assert_eq!(crc32(&[]), 0);
    }

    #[test]
    fn adler32_matches_the_check_values() {
        assert_eq!(adler32(b"Wikipedia"), 0x11e6_0398);
        assert_eq!(adler32(b""), 1);
        // A long run of 0xff keeps the sums reduced.
        let long = vec![0xffu8; 100_000];
        let (mut a, mut b) = (1u64, 0u64);
        for x in &long {
            a = (a + u64::from(*x)) % 65_521;
            b = (b + a) % 65_521;
        }
        assert_eq!(u64::from(adler32(&long)), b << 16 | a);
    }

    #[test]
    fn a_small_picture_round_trips() {
        let px: Vec<u8> = (0..2 * 3 * 4).map(|i| i as u8 * 7).collect();
        let file = encode_rgba(2, 3, &px).unwrap();
        let c = chunks(&file);
        let kinds: Vec<&[u8; 4]> = c.iter().map(|(k, _)| k).collect();
        assert_eq!(kinds, [b"IHDR", b"IDAT", b"IEND"]);
        let head = &c[0].1;
        assert_eq!(head[..4], 2u32.to_be_bytes());
        assert_eq!(head[4..8], 3u32.to_be_bytes());
        assert_eq!(head[8..], [8, 6, 0, 0, 0]);
        let raw = unzlib(&c[1].1);
        assert_eq!(raw.len(), 3 * (1 + 8));
        for (y, line) in raw.chunks(9).enumerate() {
            assert_eq!(line[0], 0);
            assert_eq!(line[1..], px[y * 8..y * 8 + 8]);
        }
        // The file ends with the fixed IEND chunk.
        assert_eq!(
            file[file.len() - 12..],
            [0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xae, 0x42, 0x60, 0x82]
        );
    }

    #[test]
    fn a_large_picture_uses_several_blocks() {
        let (w, h) = (300u32, 200u32);
        let px: Vec<u8> = (0..w * h * 4).map(|i| (i % 251) as u8).collect();
        let file = encode_rgba(w, h, &px).unwrap();
        let c = chunks(&file);
        let raw = unzlib(&c[1].1);
        assert!(raw.len() > BLOCK);
        let rows: Vec<u8> = raw
            .chunks(1 + 4 * w as usize)
            .flat_map(|l| l[1..].to_vec())
            .collect();
        assert_eq!(rows, px);
    }

    #[test]
    fn a_wrong_size_gives_none() {
        assert!(encode_rgba(0, 1, &[]).is_none());
        assert!(encode_rgba(1, 1, &[0; 3]).is_none());
        assert!(encode_rgba(2, 2, &[0; 17]).is_none());
        assert!(encode_rgba(1, 0, &[]).is_none());
    }
}
