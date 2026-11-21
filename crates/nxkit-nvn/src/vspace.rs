//! Virtual memory pools (`NVN_MEMORY_POOL_FLAGS_VIRTUAL`): GPU address ranges with no storage of their own, and the
//! page table that `nvnMemoryPoolMapVirtual` fills.
//!
//! A virtual pool gets a range of guest GPU addresses at its initialisation, in a window that the backend names and
//! that no guest memory covers ([`VirtualSpace::new`]). The game can create textures in it before it maps anything.
//! The page table holds, for every 4 KiB page of the pool, the guest GPU address of the physical page behind it (0 when
//! nothing is mapped) and the generation of the request that set it. The GPU hardware of the console translates
//! through such a table. The host cannot: the GPU sees guest memory only through the no-copy buffers of the physical
//! pools, and remapping host pages under such a buffer does not reach the GPU (`docs/nxkit-host.md`, "Guest memory").
//! So a texture of a virtual pool is read through the table instead ([`PageTables::gather`]): its bytes are gathered
//! from the physical pages, and an unmapped page reads as 0.
//!
//! [`VirtualSpace`] is the model that the ABI changes. [`VirtualSpace::snapshot`] is the immutable copy that a flush
//! hands to the backend; it costs an `Arc` clone, and a change after a snapshot copies the map of the pools and the
//! table of the pool it changes (copy on write). [`VirtualSpace::aliases`] answers the reverse question: which virtual
//! pages a CPU write to a physical page reaches, so that the textures there load again.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::error::{NvnError, Result};
use crate::space::GpuAddr;

/// The page size of a mapping request.
pub const VPAGE: u64 = 0x1000;

/// The alignment of the range of a virtual pool, the largest alignment NVN asks of a texture in a pool.
pub const VPOOL_ALIGN: u64 = 0x1_0000;

/// One page of a virtual pool.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pte {
    /// The guest GPU address of the physical page, 0 when the page is not mapped.
    pub phys: u64,
    /// The generation of the request that set the page last, 0 when none did.
    pub generation: u64,
}

/// The page table of one virtual pool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageTable {
    base: GpuAddr,
    size: u64,
    pages: Vec<Pte>,
}

impl PageTable {
    /// The first GPU address of the pool.
    pub fn base(&self) -> GpuAddr {
        self.base
    }

    /// The size of the pool in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// The page at byte offset `off` of the pool. A page past the end is unmapped.
    pub fn page(&self, off: u64) -> Pte {
        self.pages
            .get((off / VPAGE) as usize)
            .copied()
            .unwrap_or_default()
    }

    /// The newest generation of the pages that `[off, off + len)` touches, 0 when no request touched them.
    pub fn generation(&self, off: u64, len: u64) -> u64 {
        let (first, end) = page_span(off, len);
        self.pages
            .get(first.min(self.pages.len())..end.min(self.pages.len()))
            .map_or(0, |p| p.iter().map(|e| e.generation).max().unwrap_or(0))
    }
}

/// The first page and the end page of `[off, off + len)`.
fn page_span(off: u64, len: u64) -> (usize, usize) {
    let end = off.saturating_add(len).div_ceil(VPAGE);
    ((off / VPAGE) as usize, end as usize)
}

/// What a gather read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Gathered {
    /// The bytes that came from mapped pages.
    pub mapped: u64,
    /// The bytes of unmapped pages, read as 0.
    pub unmapped: u64,
    /// The bytes of mapped pages that the reader could not supply, set to 0.
    pub missing: u64,
    /// The number of reads.
    pub reads: u32,
}

/// The page tables of all virtual pools at one moment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageTables {
    pools: BTreeMap<u64, Arc<PageTable>>,
}

impl PageTables {
    /// True when no virtual pool exists.
    pub fn is_empty(&self) -> bool {
        self.pools.is_empty()
    }

    /// The pool whose range holds the GPU address `va`.
    pub fn find(&self, va: u64) -> Option<&PageTable> {
        let (_, t) = self.pools.range(..=va).next_back()?;
        (va - t.base.get() < t.size).then_some(&**t)
    }

    /// True when `va` lies in a virtual pool.
    pub fn contains(&self, va: u64) -> bool {
        self.find(va).is_some()
    }

    /// The guest GPU address of the physical byte behind `va`, None when its page is not mapped or lies in no pool.
    pub fn resolve(&self, va: u64) -> Option<u64> {
        let t = self.find(va)?;
        let off = va - t.base.get();
        let p = t.page(off);
        (p.phys != 0).then(|| p.phys + off % VPAGE)
    }

    /// Reads `out.len()` bytes of the virtual pool memory at `start` through the page table. `read(gpu, buf)` reads
    /// physical memory at a guest GPU address and says whether it could; it gets one call for each run of pages that
    /// are contiguous in physical memory. An unmapped page and a failed read give zeros. The range must lie in one
    /// virtual pool.
    pub fn gather(
        &self,
        start: GpuAddr,
        out: &mut [u8],
        mut read: impl FnMut(u64, &mut [u8]) -> bool,
    ) -> Result<Gathered> {
        let len = out.len() as u64;
        let t = self.find(start.get()).ok_or(NvnError::OutOfRange {
            addr: start.get(),
            len,
        })?;
        let off = start.get() - t.base.get();
        if off + len > t.size {
            return Err(NvnError::OutOfRange {
                addr: start.get(),
                len,
            });
        }
        let mut g = Gathered::default();
        let mut at = 0u64;
        while at < len {
            let o = off + at;
            let first = t.page(o);
            // The run: the pages from here on that are mapped (or unmapped) and contiguous in physical memory.
            let mut n = (VPAGE - o % VPAGE).min(len - at);
            while at + n < len {
                let next = t.page(o + n);
                let contiguous = if first.phys == 0 {
                    next.phys == 0
                } else {
                    next.phys == first.phys + (o + n) / VPAGE * VPAGE - o / VPAGE * VPAGE
                };
                if !contiguous {
                    break;
                }
                n += VPAGE.min(len - at - n);
            }
            let chunk = &mut out[at as usize..(at + n) as usize];
            if first.phys == 0 {
                chunk.fill(0);
                g.unmapped += n;
            } else {
                g.reads += 1;
                if read(first.phys + o % VPAGE, chunk) {
                    g.mapped += n;
                } else {
                    chunk.fill(0);
                    g.missing += n;
                }
            }
            at += n;
        }
        Ok(g)
    }
}

/// The virtual pools: the address window they take their ranges from, their page tables and the reverse map.
#[derive(Debug)]
pub struct VirtualSpace {
    start: u64,
    end: u64,
    /// Where the next reservation starts to look: ranges are taken in a ring, so a range that a finalized pool gave
    /// back is reused as late as possible.
    cursor: u64,
    /// The reserved ranges, start to length.
    used: BTreeMap<u64, u64>,
    tables: Arc<PageTables>,
    /// The mapped pages as (physical page, virtual page), for [`VirtualSpace::aliases`].
    reverse: BTreeSet<(u64, u64)>,
    generation: u64,
}

impl VirtualSpace {
    /// The space over the window `[start, start + len)` of guest GPU addresses. The window must fit in 40 bits and be
    /// [`VPOOL_ALIGN`] aligned.
    pub fn new(start: u64, len: u64) -> Result<Self> {
        if !start.is_multiple_of(VPOOL_ALIGN) || !len.is_multiple_of(VPOOL_ALIGN) || len == 0 {
            return Err(NvnError::BadPool(
                "the window of the virtual pools is empty or not aligned",
            ));
        }
        // the last byte, the end may be `1 << 40`
        GpuAddr::new(start)?.offset(len - 1)?;
        Ok(Self {
            start,
            end: start + len,
            cursor: start,
            used: BTreeMap::new(),
            tables: Arc::new(PageTables::default()),
            reverse: BTreeSet::new(),
            generation: 0,
        })
    }

    /// The window `(start, len)`.
    pub fn window(&self) -> (u64, u64) {
        (self.start, self.end - self.start)
    }

    /// True when `va` lies in the window.
    pub fn in_window(&self, va: u64) -> bool {
        (self.start..self.end).contains(&va)
    }

    /// The generation of the newest request.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The page tables as they are now, for a flush.
    pub fn snapshot(&self) -> Arc<PageTables> {
        self.tables.clone()
    }

    /// The page tables, for a look up.
    pub fn tables(&self) -> &PageTables {
        &self.tables
    }

    /// Reserves a range of `size` bytes (rounded up to [`VPOOL_ALIGN`]) for a new virtual pool with no page mapped.
    pub fn reserve(&mut self, size: u64) -> Result<GpuAddr> {
        let want = size.max(1).div_ceil(VPOOL_ALIGN) * VPOOL_ALIGN;
        let at = self
            .free_from(self.cursor, self.end, want)
            .or_else(|| self.free_from(self.start, self.cursor, want))
            .ok_or(NvnError::BadPool(
                "the window of the virtual pools has no room",
            ))?;
        self.used.insert(at, want);
        self.cursor = at + want;
        let base = GpuAddr::new(at)?;
        let pages = vec![Pte::default(); size.div_ceil(VPAGE) as usize];
        Arc::make_mut(&mut self.tables)
            .pools
            .insert(at, Arc::new(PageTable { base, size, pages }));
        Ok(base)
    }

    /// The first free start of `want` bytes in `[from, to)`.
    fn free_from(&self, from: u64, to: u64, want: u64) -> Option<u64> {
        let mut at = from;
        // A range that starts below `from` may still reach past it.
        if let Some((&s, &l)) = self.used.range(..from).next_back() {
            at = at.max(s + l);
        }
        for (&s, &l) in self.used.range(from..to) {
            if s >= at + want {
                break;
            }
            at = at.max(s + l);
        }
        (at + want <= to && at + want <= self.end).then_some(at)
    }

    /// The pool at `base` is gone: its range is free again and its pages are no longer mapped.
    pub fn release(&mut self, base: GpuAddr) {
        let b = base.get();
        if self.used.remove(&b).is_none() {
            return;
        }
        if let Some(t) = Arc::make_mut(&mut self.tables).pools.remove(&b) {
            for (i, p) in t.pages.iter().enumerate() {
                if p.phys != 0 {
                    self.reverse.remove(&(p.phys, b + i as u64 * VPAGE));
                }
            }
        }
    }

    /// Maps `[voff, voff + size)` of the pool at `base` onto the physical pages from the guest GPU address `phys`, or
    /// unmaps it when `phys` is None. Offsets, size and physical address must be [`VPAGE`] aligned, and the range must
    /// lie in the pool. The result is the virtual range that changed, as a GPU address and a length.
    pub fn map(
        &mut self,
        base: GpuAddr,
        voff: u64,
        size: u64,
        phys: Option<GpuAddr>,
    ) -> Result<(GpuAddr, u64)> {
        let b = base.get();
        let p = phys.map_or(0, GpuAddr::get);
        if !(voff | size | p).is_multiple_of(VPAGE) {
            return Err(NvnError::BadPool("a mapping request is not 4 KiB aligned"));
        }
        let tables = Arc::make_mut(&mut self.tables);
        let t = tables
            .pools
            .get_mut(&b)
            .ok_or(NvnError::BadPool("a mapping request names no virtual pool"))?;
        if voff.checked_add(size).is_none_or(|e| e > t.size) {
            return Err(NvnError::OutOfRange {
                addr: b.wrapping_add(voff),
                len: size,
            });
        }
        if phys.is_some() {
            GpuAddr::new(p)?.offset(size)?;
        }
        self.generation += 1;
        let t = Arc::make_mut(t);
        for i in 0..size / VPAGE {
            let page = ((voff / VPAGE) + i) as usize;
            let va = b + voff + i * VPAGE;
            let old = t.pages[page];
            if old.phys != 0 {
                self.reverse.remove(&(old.phys, va));
            }
            let new = if p != 0 { p + i * VPAGE } else { 0 };
            t.pages[page] = Pte {
                phys: new,
                generation: self.generation,
            };
            if new != 0 {
                self.reverse.insert((new, va));
            }
        }
        Ok((base.offset(voff)?, size))
    }

    /// The virtual ranges that show the physical bytes `[phys, phys + len)`: the ranges that a CPU write there changes.
    /// They are pushed to `out` as (GPU address, length), neighbouring pages of one run merged.
    pub fn aliases(&self, phys: u64, len: u64, out: &mut Vec<(GpuAddr, u64)>) {
        if len == 0 || self.reverse.is_empty() {
            return;
        }
        let lo = phys / VPAGE * VPAGE;
        let hi = phys.saturating_add(len);
        let mut run: Option<(u64, u64)> = None;
        for &(pp, va) in self.reverse.range((lo, 0)..(hi, 0)) {
            // the part of the page that the write touches
            let from = phys.max(pp);
            let to = hi.min(pp + VPAGE);
            let (s, e) = (va + (from - pp), va + (to - pp));
            run = match run {
                Some((rs, re)) if re == s => Some((rs, e)),
                Some((rs, re)) => {
                    push(out, rs, re);
                    Some((s, e))
                }
                None => Some((s, e)),
            };
        }
        if let Some((rs, re)) = run {
            push(out, rs, re);
        }
    }
}

fn push(out: &mut Vec<(GpuAddr, u64)>, start: u64, end: u64) {
    if let Ok(a) = GpuAddr::new(start) {
        out.push((a, end - start));
    }
}

#[cfg(test)]
mod tests;
