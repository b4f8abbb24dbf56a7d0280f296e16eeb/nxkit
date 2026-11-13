//! Random-access storages: the package file (`pread`), sub-ranges, AES-CTR and AES-CTR-EX NCA sections behind the shared
//! block cache, and BKTR indirect (relocation) storage. All are `Send + Sync` and read at offsets, so any number of
//! workers read in parallel.

#[cfg(feature = "host")]
use crate::fileext::FileExt;
#[cfg(feature = "host")]
use std::fs::File;
use std::sync::Arc;

use crate::aes::Aes128;
use crate::cache::{BlockCache, CHUNK};
use crate::error::{ExtractError, Result};

/// A readable byte range.
pub trait Storage: Send + Sync {
    /// Size in bytes.
    fn size(&self) -> u64;
    /// Fills `buf` from `offset`, reading past the end is an error.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()>;

    /// Reads `len` bytes into a new buffer.
    /// The range is checked before the buffer is allocated (a size field read from a package can be huge).
    fn read_vec(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        check_range("read", self.size(), offset, len)?;
        let mut v = vec![0u8; len];
        self.read_at(offset, &mut v)?;
        Ok(v)
    }
}

fn check_range(what: &str, size: u64, offset: u64, len: usize) -> Result<()> {
    match offset.checked_add(len as u64) {
        Some(end) if end <= size => Ok(()),
        _ => Err(ExtractError::format(format!(
            "{what}: read of {len} bytes at {offset:#x} past the end ({size:#x})"
        ))),
    }
}

/// A file read with `pread`.
#[cfg(feature = "host")]
pub struct FileStorage {
    file: File,
    size: u64,
    name: String,
}

#[cfg(feature = "host")]
impl FileStorage {
    /// Opens a file read-only.
    pub fn open(path: &std::path::Path) -> Result<FileStorage> {
        let file = File::open(path)
            .map_err(|e| ExtractError::io(format!("open {}", path.display()), e))?;
        let size = file
            .metadata()
            .map_err(|e| ExtractError::io(format!("stat {}", path.display()), e))?
            .len();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok(FileStorage { file, size, name })
    }
}

#[cfg(feature = "host")]
impl Storage for FileStorage {
    fn size(&self) -> u64 {
        self.size
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        check_range(&self.name, self.size, offset, buf.len())?;
        self.file
            .read_exact_at(buf, offset)
            .map_err(|e| ExtractError::io(format!("read {} at {offset:#x}", self.name), e))
    }
}

#[cfg(feature = "host")]
impl FileStorage {
    /// Wraps an open file of `size` bytes (a temporary file that is already unlinked).
    pub fn from_file(file: File, size: u64, name: &str) -> FileStorage {
        FileStorage {
            file,
            size,
            name: name.to_string(),
        }
    }

    /// Writes `buf` at `offset` (the file must be open for writing).
    pub fn write_at(&self, offset: u64, buf: &[u8]) -> Result<()> {
        self.file
            .write_all_at(buf, offset)
            .map_err(|e| ExtractError::io(format!("write {} at {offset:#x}", self.name), e))
    }
}

/// A window `[offset, offset + size)` of another storage.
pub struct SubStorage {
    inner: Arc<dyn Storage>,
    offset: u64,
    size: u64,
}

impl SubStorage {
    /// A window, it must lie inside `inner`.
    pub fn new(inner: Arc<dyn Storage>, offset: u64, size: u64) -> Result<SubStorage> {
        match offset.checked_add(size) {
            Some(end) if end <= inner.size() => Ok(SubStorage {
                inner,
                offset,
                size,
            }),
            _ => Err(ExtractError::format(format!(
                "range {offset:#x}+{size:#x} outside its container"
            ))),
        }
    }
}

impl Storage for SubStorage {
    fn size(&self) -> u64 {
        self.size
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        check_range("sub-range", self.size, offset, buf.len())?;
        self.inner.read_at(self.offset + offset, buf)
    }
}

/// How one part of a CTR section is keyed: the upper 8 counter bytes, or plain data.
#[derive(Clone, Copy)]
pub enum Counter {
    /// Encrypted with this upper counter.
    Upper([u8; 8]),
    /// Stored as is.
    Plain,
}

/// An AES-CTR (or CTR-EX) encrypted NCA section, decrypted through the shared [`BlockCache`] in chunks.
/// `ctr_base` is the section's offset inside its NCA (the counter counts from the NCA's start).
pub struct CtrSection {
    raw: Arc<dyn Storage>,
    key: Aes128,
    ctr_base: u64,
    /// Counter per physical range, sorted by start (one entry for a plain CTR section).
    ranges: Vec<(u64, Counter)>,
    cache: Arc<BlockCache>,
    cache_id: u32,
}

impl CtrSection {
    /// A section of `raw` (already windowed to the section) with one counter for all of it.
    pub fn new(
        raw: Arc<dyn Storage>,
        key: Aes128,
        ctr_base: u64,
        counter: Counter,
        cache: Arc<BlockCache>,
    ) -> CtrSection {
        let cache_id = cache.new_id();
        CtrSection {
            raw,
            key,
            ctr_base,
            ranges: vec![(0, counter)],
            cache,
            cache_id,
        }
    }

    /// A CTR-EX section: `ranges` (start offset, counter), sorted, the first starting at 0.
    pub fn with_ranges(
        raw: Arc<dyn Storage>,
        key: Aes128,
        ctr_base: u64,
        ranges: Vec<(u64, Counter)>,
        cache: Arc<BlockCache>,
    ) -> CtrSection {
        let cache_id = cache.new_id();
        CtrSection {
            raw,
            key,
            ctr_base,
            ranges,
            cache,
            cache_id,
        }
    }

    fn decrypt_chunk(&self, index: u64) -> Result<Vec<u8>> {
        let start = index * CHUNK;
        let len = CHUNK.min(self.raw.size() - start) as usize;
        let mut buf = crate::prof::timed(&crate::prof::COUNTERS.pread, || {
            self.raw.read_vec(start, len)
        })?;
        let end = start + len as u64;
        // apply each counter range that overlaps the chunk
        let first = self
            .ranges
            .partition_point(|r| r.0 <= start)
            .saturating_sub(1);
        for (i, &(rs, counter)) in self.ranges.iter().enumerate().skip(first) {
            if rs >= end {
                break;
            }
            let re = self.ranges.get(i + 1).map_or(u64::MAX, |r| r.0);
            let (a, b) = (rs.max(start), re.min(end));
            if a >= b {
                continue;
            }
            if let Counter::Upper(upper) = counter {
                let part = &mut buf[(a - start) as usize..(b - start) as usize];
                crate::prof::timed(&crate::prof::COUNTERS.decrypt, || {
                    self.key.ctr(upper, self.ctr_base + a, part)
                });
            }
        }
        Ok(buf)
    }
}

impl Storage for CtrSection {
    fn size(&self) -> u64 {
        self.raw.size()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        check_range("NCA section", self.size(), offset, buf.len())?;
        let mut pos = 0usize;
        while pos < buf.len() {
            let off = offset + pos as u64;
            let index = off / CHUNK;
            let within = (off % CHUNK) as usize;
            let chunk = self
                .cache
                .get(self.cache_id, index, || self.decrypt_chunk(index))?;
            let n = (chunk.len() - within).min(buf.len() - pos);
            if n == 0 {
                return Err(ExtractError::format("short chunk"));
            }
            buf[pos..pos + n].copy_from_slice(&chunk[within..within + n]);
            pos += n;
        }
        Ok(())
    }
}

/// One relocation entry of a BKTR indirect storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Relocation {
    /// Offset in the patched (virtual) section.
    pub virt: u64,
    /// Offset in the source storage.
    pub phys: u64,
    /// 0: the base NCA's section, 1: the patch NCA's section.
    pub source: u32,
}

/// The patched section: virtual offsets relocated to the base or the patch section.
pub struct IndirectStorage {
    entries: Vec<Relocation>,
    size: u64,
    sources: [Arc<dyn Storage>; 2],
}

impl IndirectStorage {
    /// `entries` sorted by `virt`, the first at 0.`size` is the table's end offset.
    pub fn new(
        entries: Vec<Relocation>,
        size: u64,
        base: Arc<dyn Storage>,
        patch: Arc<dyn Storage>,
    ) -> Result<IndirectStorage> {
        if entries.first().map(|e| e.virt) != Some(0)
            || entries.windows(2).any(|w| w[0].virt >= w[1].virt)
        {
            return Err(ExtractError::format(
                "BKTR relocation table not sorted from 0",
            ));
        }
        if entries.iter().any(|e| e.source > 1) {
            return Err(ExtractError::format(
                "BKTR relocation to an unknown storage",
            ));
        }
        Ok(IndirectStorage {
            entries,
            size,
            sources: [base, patch],
        })
    }

    /// The physical pieces `(source, phys, len)` of a virtual range (for job ordering and extent comparison).
    pub fn resolve(&self, offset: u64, len: u64) -> Vec<(u32, u64, u64)> {
        let mut out = Vec::new();
        let mut off = offset;
        let end = offset + len;
        let mut i = self
            .entries
            .partition_point(|e| e.virt <= off)
            .saturating_sub(1);
        while off < end && i < self.entries.len() {
            let e = self.entries[i];
            let next = self.entries.get(i + 1).map_or(self.size, |n| n.virt);
            let n = next.min(end) - off;
            out.push((e.source, e.phys + (off - e.virt), n));
            off += n;
            i += 1;
        }
        out
    }
}

impl Storage for IndirectStorage {
    fn size(&self) -> u64 {
        self.size
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        check_range("patched section", self.size, offset, buf.len())?;
        let mut pos = 0usize;
        for (source, phys, len) in self.resolve(offset, buf.len() as u64) {
            let n = len as usize;
            self.sources[source as usize].read_at(phys, &mut buf[pos..pos + n])?;
            pos += n;
        }
        if pos != buf.len() {
            return Err(ExtractError::format(
                "BKTR relocation table does not cover the read",
            ));
        }
        Ok(())
    }
}

/// Bytes in memory as a storage.
pub struct MemStorage(pub Vec<u8>);

impl Storage for MemStorage {
    fn size(&self) -> u64 {
        self.0.len() as u64
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        check_range("memory", self.size(), offset, buf.len())?;
        let o = offset as usize;
        buf.copy_from_slice(&self.0[o..o + buf.len()]);
        Ok(())
    }
}
