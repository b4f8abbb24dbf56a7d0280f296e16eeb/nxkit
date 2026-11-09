//! A reader for NDSR packs (`agl_ndsr*.pck`), section 3 of `docs/plans/ndsr-native.md`.
//!
//! For tests and tools only: it reads the header, the 8-byte entries and the network entries. It shares no code with
//! the build-side parser `nxb-split::engines::ndsr_shaders`.

use std::fmt;

/// Offset of the first entry.
pub const ENTRIES_AT: usize = 0x14;

/// Entry types (section 3).
pub mod kind {
    /// Compute program control section.
    pub const COMPUTE_CONTROL: u8 = 6;
    /// Compute program code.
    pub const COMPUTE_CODE: u8 = 7;
    /// Network weights, f16.
    pub const WEIGHTS: u8 = 13;
    /// The 4-byte config before each network program.
    pub const CONFIG: u8 = 14;
}

/// Why a pack did not parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Shorter than the header or the entry table.
    Truncated,
    /// The first four bytes are not `NDSR`.
    BadMagic,
    /// An entry reaches past the end of the pack.
    EntryOutOfRange(usize),
    /// A network program has no config or no weights before it, or no code after it.
    BadNetwork(usize),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated => write!(f, "the pack is truncated"),
            Error::BadMagic => write!(f, "not an NDSR pack"),
            Error::EntryOutOfRange(i) => write!(f, "entry {i} reaches past the end of the pack"),
            Error::BadNetwork(i) => write!(f, "network program at entry {i} is malformed"),
        }
    }
}

impl std::error::Error for Error {}

/// One entry of the table: `(u32 offset, u32 length << 4 | type)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Offset from the start of the pack.
    pub offset: u32,
    /// Length in bytes.
    pub len: u32,
    /// The type (4 bits).
    pub kind: u8,
}

/// A network program found in the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetworkEntry {
    /// Index of its control entry.
    pub index: usize,
    /// The config `b0 b1 b2 b3`.
    pub config: [u8; 4],
    /// The control section.
    pub control: Entry,
    /// The code.
    pub code: Entry,
    /// The weights that precede it.
    pub weights: Entry,
}

/// A parsed pack.
#[derive(Debug)]
pub struct PackFile<'a> {
    bytes: &'a [u8],
    /// The header words at `+4` to `+0x10`.
    pub header: [u32; 4],
    /// The entries.
    pub entries: Vec<Entry>,
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

impl<'a> PackFile<'a> {
    /// Parse the header and the entry table. Every entry must lie inside `bytes`.
    pub fn parse(bytes: &'a [u8]) -> Result<PackFile<'a>, Error> {
        if bytes.len() < ENTRIES_AT {
            return Err(Error::Truncated);
        }
        if &bytes[..4] != b"NDSR" {
            return Err(Error::BadMagic);
        }
        let header = [
            u32_at(bytes, 4),
            u32_at(bytes, 8),
            u32_at(bytes, 12),
            u32_at(bytes, 16),
        ];
        let n = header[2] as usize;
        let end = n.checked_mul(8).and_then(|t| t.checked_add(ENTRIES_AT));
        if end.is_none_or(|e| e > bytes.len()) {
            return Err(Error::Truncated);
        }
        let mut entries = Vec::with_capacity(n);
        for i in 0..n {
            let at = ENTRIES_AT + 8 * i;
            let (offset, lt) = (u32_at(bytes, at), u32_at(bytes, at + 4));
            let e = Entry {
                offset,
                len: lt >> 4,
                kind: (lt & 15) as u8,
            };
            if u64::from(e.offset) + u64::from(e.len) > bytes.len() as u64 {
                return Err(Error::EntryOutOfRange(i));
            }
            entries.push(e);
        }
        Ok(PackFile {
            bytes,
            header,
            entries,
        })
    }

    /// The bytes of an entry.
    pub fn data(&self, e: &Entry) -> &'a [u8] {
        &self.bytes[e.offset as usize..(e.offset + e.len) as usize]
    }

    /// The network programs in table order: a compute control section that follows a config entry. The weights are
    /// the last weights entry before it (the three variants of one blob each carry the same weights entry). Programs
    /// that share their control and code with an earlier one are listed again; callers deduplicate.
    pub fn networks(&self) -> Result<Vec<NetworkEntry>, Error> {
        let mut out = Vec::new();
        let (mut config, mut weights) = (None, None);
        for (i, e) in self.entries.iter().enumerate() {
            match e.kind {
                kind::CONFIG if e.len == 4 => {
                    let d = self.data(e);
                    config = Some([d[0], d[1], d[2], d[3]]);
                }
                kind::WEIGHTS => weights = Some(*e),
                kind::COMPUTE_CONTROL if config.is_some() => {
                    let code = self
                        .entries
                        .get(i + 1)
                        .filter(|c| c.kind == kind::COMPUTE_CODE)
                        .ok_or(Error::BadNetwork(i))?;
                    let weights = weights.ok_or(Error::BadNetwork(i))?;
                    let config = config.take().ok_or(Error::BadNetwork(i))?;
                    out.push(NetworkEntry {
                        index: i,
                        config,
                        control: *e,
                        code: *code,
                        weights,
                    });
                }
                _ => {}
            }
        }
        Ok(out)
    }

    /// The network programs without repeats: the first of each distinct pair of control and code offsets.
    pub fn distinct_networks(&self) -> Result<Vec<NetworkEntry>, Error> {
        let mut seen = Vec::new();
        let mut out = Vec::new();
        for n in self.networks()? {
            let id = (n.control.offset, n.code.offset);
            if !seen.contains(&id) {
                seen.push(id);
                out.push(n);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pack of two networks sharing one weights entry, one non-network program, and a repeated entry.
    fn synthetic() -> Vec<u8> {
        let mut data = vec![0u8; 0x200];
        data[0x100..0x104].copy_from_slice(&[1, 2, 3, 4]);
        data[0x104..0x108].copy_from_slice(&[5, 6, 7, 8]);
        let e = |o: u32, l: u32, t: u32| (o, l << 4 | t);
        let ents = [
            e(0x40, 4, 9),
            e(0x50, 0xe0, 6),
            e(0x60, 0x10, 7),
            e(0x100, 4, 14),
            e(0x180, 0x20, 13),
            e(0x70, 0xe0, 6),
            e(0x80, 0x10, 7),
            e(0x104, 4, 14),
            e(0x180, 0x20, 13),
            e(0x70, 0xe0, 6),
            e(0x80, 0x10, 7),
            e(0x108, 4, 14),
            e(0x180, 0x20, 13),
            e(0x90, 0xe0, 6),
            e(0xa0, 0x10, 7),
        ];
        let mut p = b"NDSR".to_vec();
        for w in [0x10100, 0x20302, ents.len() as u32, 0x20c00] {
            p.extend(w.to_le_bytes());
        }
        for (o, lt) in ents {
            p.extend(o.to_le_bytes());
            p.extend(lt.to_le_bytes());
        }
        let n = p.len();
        p.extend(&data[n..]);
        p[0x100..0x104].copy_from_slice(&[1, 2, 3, 4]);
        p[0x104..0x108].copy_from_slice(&[5, 6, 7, 8]);
        p
    }

    #[test]
    fn reads_the_header_and_entries() {
        let p = synthetic();
        let f = PackFile::parse(&p).unwrap();
        assert_eq!(f.header, [0x10100, 0x20302, 15, 0x20c00]);
        assert_eq!(
            f.entries[3],
            Entry {
                offset: 0x100,
                len: 4,
                kind: 14
            }
        );
        assert_eq!(f.data(&f.entries[3]), &[1, 2, 3, 4]);
    }

    #[test]
    fn finds_the_networks() {
        let p = synthetic();
        let f = PackFile::parse(&p).unwrap();
        let all = f.networks().unwrap();
        // The first compute program has no config (its tile entry is type 9), so it is not a network.
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].config, [1, 2, 3, 4]);
        assert_eq!(all[0].weights.offset, 0x180);
        let d = f.distinct_networks().unwrap();
        assert_eq!(d.len(), 2);
        assert_eq!((d[0].control.offset, d[1].control.offset), (0x70, 0x90));
        assert_eq!(d[1].config, [0, 0, 0, 0]);
    }

    #[test]
    fn rejects_bad_packs() {
        let mut p = synthetic();
        assert_eq!(PackFile::parse(&p[..8]).unwrap_err(), Error::Truncated);
        assert_eq!(PackFile::parse(&p[..40]).unwrap_err(), Error::Truncated);
        assert_eq!(
            PackFile::parse(&p[..0x100]).unwrap_err(),
            Error::EntryOutOfRange(1)
        );
        p[0] = b'X';
        assert_eq!(PackFile::parse(&p).unwrap_err(), Error::BadMagic);
    }
}
