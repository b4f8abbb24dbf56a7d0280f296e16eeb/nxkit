//! Digests and the hash-once types: `Digest`, `FileDigest`, `HashingWriter`.

use crate::sha256::Sha256;
use std::fmt;
use std::io::{self, Write};

/// A SHA-256 digest. Displays as 64 lower-case hex characters.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest(pub [u8; 32]);

/// A string that is not a valid id or digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// What was expected, for example `64 hex characters`.
    pub expected: &'static str,
    /// The rejected input, cut to 80 characters.
    pub input: String,
}

impl ParseError {
    pub(crate) fn new(expected: &'static str, input: &str) -> Self {
        ParseError {
            expected,
            input: input.chars().take(80).collect(),
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "expected {}, got {:?}", self.expected, self.input)
    }
}

impl std::error::Error for ParseError {}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Lower-case hex of bytes.
pub fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[usize::from(b >> 4)] as char);
        s.push(HEX[usize::from(b & 15)] as char);
    }
    s
}

/// Parse exactly `out.len() * 2` lower-case hex characters into `out`. Upper case is refused: ids are canonical.
pub(crate) fn from_hex_into(s: &str, out: &mut [u8]) -> bool {
    let b = s.as_bytes();
    if b.len() != out.len() * 2 {
        return false;
    }
    fn nib(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        }
    }
    for (i, o) in out.iter_mut().enumerate() {
        match (nib(b[2 * i]), nib(b[2 * i + 1])) {
            (Some(h), Some(l)) => *o = (h << 4) | l,
            _ => return false,
        }
    }
    true
}

impl Digest {
    /// Parse 64 lower-case hex characters.
    pub fn from_hex(s: &str) -> Result<Digest, ParseError> {
        let mut out = [0u8; 32];
        if from_hex_into(s, &mut out) {
            Ok(Digest(out))
        } else {
            Err(ParseError::new("64 lower-case hex characters", s))
        }
    }

    /// 64 lower-case hex characters.
    pub fn to_hex(&self) -> String {
        to_hex(&self.0)
    }

    /// The raw bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Digest({})", self.to_hex())
    }
}

/// Length and SHA-256 of a payload, computed once by its producer (design-pipeline-rest section 8).
///
/// SHA-1 may join as an optional field (decisions.md W3).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct FileDigest {
    /// Length in bytes.
    pub len: u64,
    /// SHA-256 of the bytes.
    pub sha256: Digest,
}

impl FileDigest {
    /// Digest of bytes in memory.
    pub fn of(bytes: &[u8]) -> FileDigest {
        FileDigest {
            len: bytes.len() as u64,
            sha256: crate::sha256::sha256(bytes),
        }
    }
}

/// A writer that hashes everything written through it: the producer hashes while writing, nobody re-reads to hash.
///
/// ```
/// use std::io::Write;
/// use nxb_model::{FileDigest, HashingWriter};
/// let mut w = HashingWriter::new(Vec::new());
/// w.write_all(b"hello").unwrap();
/// let (bytes, digest) = w.finish();
/// assert_eq!(digest, FileDigest::of(&bytes));
/// ```
#[derive(Debug)]
pub struct HashingWriter<W: Write> {
    inner: W,
    hasher: Sha256,
}

impl<W: Write> HashingWriter<W> {
    /// Wrap a writer.
    pub fn new(inner: W) -> Self {
        HashingWriter {
            inner,
            hasher: Sha256::new(),
        }
    }

    /// The inner writer and the digest of everything written.
    pub fn finish(self) -> (W, FileDigest) {
        let len = self.hasher.len();
        let digest = FileDigest {
            len,
            sha256: self.hasher.finalize(),
        };
        (self.inner, digest)
    }

    /// The inner writer, for calls such as `sync_all`.
    pub fn get_ref(&self) -> &W {
        &self.inner
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxb_testkit::check;

    #[test]
    fn hex_round_trip() {
        check(11, 200, |r| {
            let mut b = [0u8; 32];
            b.copy_from_slice(&r.bytes(32));
            let d = Digest(b);
            assert_eq!(Digest::from_hex(&d.to_hex()), Ok(d));
        });
    }

    #[test]
    fn hex_rejects_bad_input() {
        assert!(Digest::from_hex("").is_err());
        assert!(Digest::from_hex(&"A".repeat(64)).is_err());
        assert!(Digest::from_hex(&"0".repeat(63)).is_err());
        assert!(Digest::from_hex(&"g".repeat(64)).is_err());
        assert!(Digest::from_hex(&"é".repeat(32)).is_err());
    }

    #[test]
    fn hashing_writer_matches_one_shot() {
        check(12, 50, |r| {
            let n = r.below(5000) as usize;
            let data = r.bytes(n);
            let mut w = HashingWriter::new(Vec::new());
            for part in data.chunks(1 + r.below(300) as usize) {
                w.write_all(part).unwrap();
            }
            let (out, d) = w.finish();
            assert_eq!(out, data);
            assert_eq!(d, FileDigest::of(&data));
        });
    }
}
