//! Reading and changing the index of a `DMROMFS1` image, as bytes (the caller does the file I/O).
//!
//! The image is a 32-byte header (magic, index offset, index length, entry count), the file data, then the index:
//! 24 bytes per entry (`a`, `b`, name offset, name length, kind, damaged flag) and the name table. Entry 0 is the
//! root. A directory's `a` is its first child and `b` the number of children. A file's `a` is its offset in the
//! image and `b` its size. Readers take every offset from the index, so a file may move to the end of the data.

use std::fmt;

/// The magic at the start of an image.
pub const MAGIC: &[u8; 8] = b"DMROMFS1";

/// What is wrong with an image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageError(pub String);

impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ImageError {}

fn bad(m: &str) -> ImageError {
    ImageError(format!("DMROMFS1 image: {m}"))
}

/// The image header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// Where the index starts (the end of the file data).
    pub index_offset: u64,
    /// Bytes of the index and name table.
    pub index_len: u64,
    /// Number of entries.
    pub entries: u64,
}

impl Header {
    /// Reads the 32-byte header.
    pub fn parse(h: &[u8]) -> Result<Header, ImageError> {
        if h.get(..8) != Some(MAGIC.as_slice()) || h.len() < 32 {
            return Err(bad("not an image"));
        }
        let q = |o: usize| u64::from_le_bytes(h[o..o + 8].try_into().unwrap_or([0; 8]));
        Ok(Header {
            index_offset: q(8),
            index_len: q(16),
            entries: q(24),
        })
    }

    /// The 32 header bytes.
    pub fn bytes(&self) -> [u8; 32] {
        let mut h = [0u8; 32];
        h[..8].copy_from_slice(MAGIC);
        h[8..16].copy_from_slice(&self.index_offset.to_le_bytes());
        h[16..24].copy_from_slice(&self.index_len.to_le_bytes());
        h[24..32].copy_from_slice(&self.entries.to_le_bytes());
        h
    }
}

/// A file found in the index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileRef {
    /// Its entry number.
    pub entry: usize,
    /// Offset in the image.
    pub offset: u64,
    /// Size.
    pub size: u64,
}

/// The index and name table of an image.
#[derive(Clone, Debug)]
pub struct Index {
    bytes: Vec<u8>,
    n: usize,
}

impl Index {
    /// The index bytes read at [`Header::index_offset`] (all [`Header::index_len`] of them).
    pub fn parse(header: &Header, bytes: Vec<u8>) -> Result<Index, ImageError> {
        let n = usize::try_from(header.entries).map_err(|_| bad("entry count"))?;
        if n == 0 || bytes.len() < n.saturating_mul(24) {
            return Err(bad("damaged index"));
        }
        Ok(Index { bytes, n })
    }

    fn entry(&self, k: usize) -> Option<(u64, u64, &[u8], bool)> {
        if k >= self.n {
            return None;
        }
        let e = &self.bytes[k * 24..k * 24 + 24];
        let q = |o: usize| u64::from_le_bytes(e[o..o + 8].try_into().unwrap_or([0; 8]));
        let no = u32::from_le_bytes(e[16..20].try_into().unwrap_or([0; 4])) as usize;
        let nl = u16::from_le_bytes([e[20], e[21]]) as usize;
        let pool = self.n * 24;
        let name = self.bytes.get(pool + no..pool + no + nl)?;
        Some((q(0), q(8), name, e[22] == 1))
    }

    /// The file at `path` (`/` separated, with or without a leading `/`), or `None` when there is none.
    pub fn find(&self, path: &str) -> Option<FileRef> {
        let mut at = 0usize;
        let mut parts = path.trim_start_matches('/').split('/').peekable();
        while let Some(part) = parts.next() {
            let (first, count, _, file) = self.entry(at)?;
            if file {
                return None;
            }
            let first = usize::try_from(first).ok()?;
            let count = usize::try_from(count).ok()?;
            at = (first..first.checked_add(count)?)
                .find(|&k| self.entry(k).is_some_and(|e| e.2 == part.as_bytes()))?;
            let (offset, size, _, file) = self.entry(at)?;
            if parts.peek().is_none() {
                return file.then_some(FileRef {
                    entry: at,
                    offset,
                    size,
                });
            }
        }
        None
    }

    /// Moves a file: its entry gets a new offset and size. The name table stays the same.
    pub fn set_file(&mut self, entry: usize, offset: u64, size: u64) -> Result<(), ImageError> {
        if self.entry(entry).is_none_or(|e| !e.3) {
            return Err(bad(&format!("entry {entry} is not a file")));
        }
        let e = &mut self.bytes[entry * 24..entry * 24 + 16];
        e[..8].copy_from_slice(&offset.to_le_bytes());
        e[8..].copy_from_slice(&size.to_le_bytes());
        Ok(())
    }

    /// The index and name table, as written at the index offset.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `/a` (dir) with `/a/x` (3 bytes) and `/b` (2 bytes).
    fn sample() -> (Header, Index) {
        let mut idx = Vec::new();
        let mut e = |a: u64, b: u64, no: u32, nl: u16, file: bool| {
            idx.extend_from_slice(&a.to_le_bytes());
            idx.extend_from_slice(&b.to_le_bytes());
            idx.extend_from_slice(&no.to_le_bytes());
            idx.extend_from_slice(&nl.to_le_bytes());
            idx.extend_from_slice(&[u8::from(file), 0]);
        };
        e(1, 2, 0, 0, false);
        e(3, 1, 0, 1, false);
        e(32, 2, 1, 1, true);
        e(34, 3, 2, 1, true);
        idx.extend_from_slice(b"abx");
        let h = Header {
            index_offset: 37,
            index_len: idx.len() as u64,
            entries: 4,
        };
        let i = Index::parse(&h, idx).unwrap();
        (h, i)
    }

    #[test]
    fn files_are_found_by_path() {
        let (h, i) = sample();
        assert_eq!(Header::parse(&h.bytes()), Ok(h));
        assert_eq!(
            i.find("/a/x"),
            Some(FileRef {
                entry: 3,
                offset: 34,
                size: 3
            })
        );
        assert_eq!(i.find("b").map(|f| f.entry), Some(2));
        assert_eq!(i.find("/a"), None);
        assert_eq!(i.find("/a/y"), None);
        assert_eq!(i.find("/b/x"), None);
    }

    #[test]
    fn a_moved_file_is_found_at_its_new_place() {
        let (_, mut i) = sample();
        i.set_file(3, 100, 9).unwrap();
        assert_eq!(i.find("/a/x").map(|f| (f.offset, f.size)), Some((100, 9)));
        assert!(i.set_file(1, 0, 0).is_err());
    }
}
