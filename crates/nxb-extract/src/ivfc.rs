//! Hash-checked views of a decrypted section: IVFC (RomFS sections, LibHac's `IntegrityVerificationStorage` with
//! `ErrorOnInvalid`) and hierarchical SHA-256 (PFS0 sections).
//!
//! IVFC: the hash levels are small (32 bytes per 16 KiB block: 7.3 MB for a 3.7 GB RomFS), they are read and checked
//! level by level at open. Data blocks are checked on first read, a bitset remembers checked blocks, so a block that
//! several files share is hashed once per version.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use nxb_model::sha256;

use crate::error::{ExtractError, Result};
use crate::nca::{FsHeader, u32le, u64le};
use crate::storage::Storage;

/// How the last, partial block of a level is hashed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tail {
    /// Zero-padded to the block size.
    Padded,
    /// Only the bytes present.
    Truncated,
}

fn block_hash(data: &[u8], bs: usize, tail: Tail) -> [u8; 32] {
    if data.len() < bs && tail == Tail::Padded {
        let mut v = data.to_vec();
        v.resize(bs, 0);
        sha256(&v).0
    } else {
        sha256(data).0
    }
}

/// The checked data level of an IVFC section.
pub struct IvfcStorage {
    data: Arc<dyn Storage>,
    data_offset: u64,
    size: u64,
    bs: u64,
    hashes: Vec<u8>,
    tail: Tail,
    checked: Vec<AtomicU64>,
    /// `--allow-damaged`: a block that fails its hash is returned as read and remembered in `bad`.
    lenient: bool,
    bad: std::sync::Mutex<std::collections::BTreeSet<u64>>,
}

fn check_level(level: &[u8], bs: usize, expected: &[u8], tail: Tail) -> bool {
    level
        .chunks(bs)
        .enumerate()
        .all(|(k, b)| expected.get(32 * k..32 * k + 32) == Some(&block_hash(b, bs, tail)[..]))
}

impl IvfcStorage {
    /// An empty checked view for a Program that has no data section, not for an invalid IVFC tree.
    pub(crate) fn empty() -> Self {
        Self {
            data: Arc::new(crate::storage::MemStorage(Vec::new())),
            data_offset: 0,
            size: 0,
            bs: 1,
            hashes: Vec::new(),
            tail: Tail::Padded,
            checked: Vec::new(),
            lenient: false,
            bad: std::sync::Mutex::new(std::collections::BTreeSet::new()),
        }
    }

    /// Opens the IVFC levels described by `fs` over the decrypted section `section`.
    pub fn open(section: Arc<dyn Storage>, fs: &FsHeader) -> Result<IvfcStorage> {
        Self::open_with(section, fs, false)
    }

    /// Like [`IvfcStorage::open`]; with `lenient` a data block that fails its hash is read as it is and listed by
    /// [`IvfcStorage::damaged_in`] (the hash levels above the data must still pass).
    pub fn open_with(
        section: Arc<dyn Storage>,
        fs: &FsHeader,
        lenient: bool,
    ) -> Result<IvfcStorage> {
        let h = &fs.raw;
        if fs.hash_type != 3 || h.get(8..12) != Some(b"IVFC") {
            return Err(ExtractError::format("section has no IVFC hash tree"));
        }
        let levels = u32le(h, 0x14) as usize;
        if !(2..=7).contains(&levels) {
            return Err(ExtractError::format("IVFC level count"));
        }
        let lv: Vec<(u64, u64, u64)> = (0..levels - 1)
            .map(|i| {
                let o = 0x18 + 0x18 * i;
                (
                    u64le(h, o),
                    u64le(h, o + 8),
                    1u64 << (u32le(h, o + 16) & 31),
                )
            })
            .collect();
        let master = h
            .get(0xC8..0xC8 + 0x20)
            .ok_or_else(|| ExtractError::format("IVFC master hash"))?
            .to_vec();
        let mut expected = master;
        let mut tail = Tail::Padded;
        let n = lv.len();
        for (i, &(off, size, bs)) in lv.iter().take(n - 1).enumerate() {
            if size > (256 << 20) {
                return Err(ExtractError::format("IVFC hash level too large"));
            }
            let level = section.read_vec(off, size as usize)?;
            if i == 0 && !check_level(&level, bs as usize, &expected, tail) {
                tail = Tail::Truncated;
            }
            if !check_level(&level, bs as usize, &expected, tail) {
                return Err(ExtractError::Damaged(vec![format!(
                    "(IVFC hash level {})",
                    i + 1
                )]));
            }
            expected = level;
        }
        let (data_offset, size, bs) = lv[n - 1];
        if data_offset
            .checked_add(size)
            .is_none_or(|e| e > section.size())
            || !(16..=(1 << 24)).contains(&bs)
        {
            return Err(ExtractError::format("IVFC data level outside the section"));
        }
        let blocks = size.div_ceil(bs);
        if (expected.len() as u64) < blocks * 32 {
            return Err(ExtractError::format(
                "IVFC hash level shorter than the data level",
            ));
        }
        let checked = (0..blocks.div_ceil(64))
            .map(|_| AtomicU64::new(0))
            .collect();
        Ok(IvfcStorage {
            data: section,
            data_offset,
            size,
            bs,
            hashes: expected,
            tail,
            checked,
            lenient,
            bad: std::sync::Mutex::new(std::collections::BTreeSet::new()),
        })
    }

    /// Reads (and so checks) the whole data level of a lenient storage. Returns the number of damaged blocks.
    pub fn scan(&self) -> Result<usize> {
        let mut buf = vec![0u8; (self.bs as usize) * 64];
        let mut off = 0u64;
        while off < self.size {
            let n = (buf.len() as u64).min(self.size - off) as usize;
            self.read_at(off, &mut buf[..n])?;
            off += n as u64;
        }
        Ok(self.bad.lock().map(|b| b.len()).unwrap_or(0))
    }

    /// True when a damaged block (found by earlier reads) overlaps `[offset, offset + len)`.
    pub fn damaged_in(&self, offset: u64, len: u64) -> bool {
        if len == 0 {
            return false;
        }
        let (a, b) = (offset / self.bs, (offset + len - 1) / self.bs);
        self.bad
            .lock()
            .map(|s| s.range(a..=b).next().is_some())
            .unwrap_or(false)
    }

    fn is_checked(&self, b: u64) -> bool {
        self.checked[(b / 64) as usize].load(Ordering::Acquire) & (1 << (b % 64)) != 0
    }

    /// Bytes of data hashed so far are not tracked, this returns how many blocks were checked.
    pub fn checked_blocks(&self) -> u64 {
        self.checked
            .iter()
            .map(|w| w.load(Ordering::Relaxed).count_ones() as u64)
            .sum()
    }
}

impl Storage for IvfcStorage {
    fn size(&self) -> u64 {
        self.size
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let end = offset
            .checked_add(buf.len() as u64)
            .filter(|&e| e <= self.size);
        let Some(end) = end else {
            return Err(ExtractError::format("RomFS read past the end"));
        };
        let mut b = offset / self.bs;
        let mut pos = 0usize;
        while pos < buf.len() {
            let bstart = b * self.bs;
            let blen = self.bs.min(self.size - bstart);
            let (from, to) = (offset.max(bstart), end.min(bstart + blen));
            let dst = &mut buf[pos..pos + (to - from) as usize];
            if self.is_checked(b) {
                self.data.read_at(self.data_offset + from, dst)?;
            } else {
                let block = self
                    .data
                    .read_vec(self.data_offset + bstart, blen as usize)?;
                let hash = crate::prof::timed(&crate::prof::COUNTERS.verify, || {
                    block_hash(&block, self.bs as usize, self.tail)
                });
                if self.hashes[(32 * b) as usize..(32 * b + 32) as usize] != hash {
                    if !self.lenient {
                        return Err(ExtractError::Damaged(Vec::new()));
                    }
                    if let Ok(mut s) = self.bad.lock() {
                        s.insert(b);
                    }
                }
                self.checked[(b / 64) as usize].fetch_or(1 << (b % 64), Ordering::AcqRel);
                dst.copy_from_slice(&block[(from - bstart) as usize..(to - bstart) as usize]);
            }
            pos += dst.len();
            b += 1;
        }
        Ok(())
    }
}

/// Reads and checks the data layer of a hierarchical SHA-256 section (PFS0: ExeFS, logos) into memory.
pub fn read_sha256_section(section: &dyn Storage, fs: &FsHeader) -> Result<Vec<u8>> {
    let h = &fs.raw;
    if fs.hash_type != 2 {
        return Err(ExtractError::format(
            "section has no hierarchical SHA-256 hash",
        ));
    }
    let master = h
        .get(0x8..0x28)
        .ok_or_else(|| ExtractError::format("hash header"))?;
    let bs = u32le(h, 0x28) as usize;
    let layers = u32le(h, 0x2C) as usize;
    if layers != 2 || bs == 0 {
        return Err(ExtractError::Unsupported(format!(
            "hierarchical SHA-256 with {layers} layers"
        )));
    }
    let (toff, tsize) = (u64le(h, 0x30), u64le(h, 0x38));
    let (doff, dsize) = (u64le(h, 0x40), u64le(h, 0x48));
    if tsize > (64 << 20) || dsize > (4 << 30) {
        return Err(ExtractError::format("hierarchical SHA-256 layer sizes"));
    }
    let table = section.read_vec(toff, tsize as usize)?;
    if sha256(&table).0[..] != master[..] {
        return Err(ExtractError::Damaged(vec![
            "(hash table of a PFS0 section)".into(),
        ]));
    }
    let data = section.read_vec(doff, dsize as usize)?;
    for (k, b) in data.chunks(bs).enumerate() {
        if table.get(32 * k..32 * k + 32) != Some(&sha256(b).0[..]) {
            return Err(ExtractError::Damaged(vec![format!("(PFS0 block {k})")]));
        }
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_section_has_no_readable_data() {
        let empty = IvfcStorage::empty();
        assert_eq!(empty.size(), 0);
        assert_eq!(empty.scan().ok(), Some(0));
        assert_eq!(empty.checked_blocks(), 0);
        assert!(empty.read_at(0, &mut []).is_ok());
        assert!(empty.read_at(0, &mut [0]).is_err());
        assert!(empty.read_at(1, &mut []).is_err());
    }
}
