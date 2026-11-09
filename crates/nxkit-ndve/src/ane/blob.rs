//! The `weights/weight.bin` file of a Core ML ML Program.
//!
//! Layout (read from the output of coremltools): a 64-byte file header (`u32` entry count, `u32` 2, zeros), then per
//! entry a 64-byte header (`u32` 0xDEADBEEF, `u32` 1 = f16, `u64` data size, `u64` absolute offset of the data, zeros)
//! followed by the data, padded with zeros to a multiple of 64 bytes. A `blobFileValue` of the model points at the
//! entry header.

/// Bytes of the file header and of an entry header; also the alignment of an entry.
pub const ALIGN: usize = 64;
const MAGIC: u32 = 0xdead_beef;
const KIND_F16: u32 = 1;

/// Where an entry went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The name of the constant the entry belongs to.
    pub name: String,
    /// Offset of the entry header in the file (what the model stores).
    pub offset: usize,
    /// Bytes of data (without header and padding).
    pub len: usize,
}

/// A weight file being written.
#[derive(Debug)]
pub struct BlobWriter {
    bytes: Vec<u8>,
    entries: Vec<Entry>,
}

impl Default for BlobWriter {
    fn default() -> Self {
        BlobWriter::new()
    }
}

impl BlobWriter {
    /// A file with a header and no entries.
    #[must_use]
    pub fn new() -> BlobWriter {
        BlobWriter {
            bytes: vec![0; ALIGN],
            entries: Vec::new(),
        }
    }

    /// Append `data` (f16, little endian) as the entry of constant `name`; returns the offset of the entry header.
    pub fn add(&mut self, name: &str, data: &[u8]) -> usize {
        let at = self.bytes.len();
        debug_assert_eq!(at % ALIGN, 0);
        let mut head = [0u8; ALIGN];
        head[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        head[4..8].copy_from_slice(&KIND_F16.to_le_bytes());
        head[8..16].copy_from_slice(&(data.len() as u64).to_le_bytes());
        head[16..24].copy_from_slice(&((at + ALIGN) as u64).to_le_bytes());
        self.bytes.extend_from_slice(&head);
        self.bytes.extend_from_slice(data);
        self.bytes
            .resize(self.bytes.len().next_multiple_of(ALIGN), 0);
        self.entries.push(Entry {
            name: name.to_owned(),
            offset: at,
            len: data.len(),
        });
        at
    }

    /// The finished file and the entry table.
    #[must_use]
    pub fn finish(mut self) -> (Vec<u8>, Vec<Entry>) {
        let count = self.entries.len() as u32;
        self.bytes[0..4].copy_from_slice(&count.to_le_bytes());
        self.bytes[4..8].copy_from_slice(&2u32.to_le_bytes());
        (self.bytes, self.entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_are_aligned_and_self_describing() {
        let mut w = BlobWriter::new();
        let a = w.add("a", &[1u8; 32]);
        let b = w.add("b", &[2u8; 100]);
        let c = w.add("c", &[3u8; 64]);
        assert_eq!((a, b, c), (64, 192, 384));
        let (bytes, entries) = w.finish();
        assert_eq!(bytes.len() % ALIGN, 0);
        assert_eq!(
            u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            3
        );
        for e in &entries {
            assert_eq!(e.offset % ALIGN, 0);
            assert_eq!(&bytes[e.offset..e.offset + 4], &MAGIC.to_le_bytes());
            let size = u64::from_le_bytes(bytes[e.offset + 8..e.offset + 16].try_into().unwrap());
            let data = u64::from_le_bytes(bytes[e.offset + 16..e.offset + 24].try_into().unwrap());
            assert_eq!(size as usize, e.len);
            assert_eq!(data as usize, e.offset + ALIGN);
        }
        assert_eq!(bytes.len(), c + 64 + 64);
    }
}
