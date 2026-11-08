//! A zstd frame of raw blocks: the bytes as they are, in a valid frame, with no compressor.
//!
//! The frame names no dictionary, so a decoder that is given one (as a game's dictionary decoder is) still accepts
//! it: raw blocks never reach into a dictionary. The frame has its content size and no checksum.

/// The largest block a frame may hold (RFC 8878 3.1.1.2.4).
const BLOCK_MAX: usize = 128 * 1024;

/// One zstd frame that holds `data` in raw (stored) blocks. [`super::decompress`] gives `data` back.
pub fn raw_frame(data: &[u8]) -> Vec<u8> {
    let blocks = data.len().div_ceil(BLOCK_MAX).max(1);
    let mut out = Vec::with_capacity(data.len() + 13 + 3 * blocks);
    out.extend_from_slice(&[0x28, 0xb5, 0x2f, 0xfd]);
    // frame header: an 8-byte content size, single segment (no window descriptor), no checksum, no dictionary ID
    out.push(0xe0);
    out.extend_from_slice(&(data.len() as u64).to_le_bytes());
    let mut chunks = data.chunks(BLOCK_MAX).peekable();
    if chunks.peek().is_none() {
        // an empty frame still has one block, the last one
        out.extend_from_slice(&[1, 0, 0]);
        return out;
    }
    while let Some(c) = chunks.next() {
        let last = u32::from(chunks.peek().is_none());
        // block header: last flag, type 0 (raw), size
        let h = last | (c.len() as u32) << 3;
        out.extend_from_slice(&h.to_le_bytes()[..3]);
        out.extend_from_slice(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_raw_frame_decodes_to_its_input() {
        for n in [
            0usize,
            1,
            1000,
            BLOCK_MAX,
            BLOCK_MAX + 1,
            3 * BLOCK_MAX + 17,
        ] {
            let data: Vec<u8> = (0..n).map(|i| (i * 31 % 251) as u8).collect();
            let f = raw_frame(&data);
            assert_eq!(super::super::decompress(&f).unwrap(), data, "{n} bytes");
            assert_eq!(super::super::dict_id(&f), 0);
        }
    }
}
