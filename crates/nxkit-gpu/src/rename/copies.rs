//! Where a frame finds its copies: the start values of packed words and the map of the renamed ranges.

use super::*;

impl CapRange {
    /// The bytes of the word `w` at the flush, for a word of `cmask` of a merge range.
    pub(crate) fn cvalue(&self, w: usize) -> Option<u32> {
        let block = *self.cmask.get(w / 32)?;
        if block >> (w % 32) & 1 == 0 {
            return None;
        }
        let before = (block & ((1u32 << (w % 32)) - 1)).count_ones();
        self.cvals
            .get((self.cprefix.get(w / 32)? + before) as usize)
            .copied()
    }
}

impl RenameMap {
    /// A map without a range.
    pub(crate) const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// The map of `entries`, `(guest address, size, real address of the copy)`.
    pub(crate) fn from_entries(mut entries: Vec<(u64, u32, u64)>) -> Self {
        entries.sort_unstable();
        Self { entries }
    }

    /// The real address of the binding `[addr, addr + size)` when it lies inside a renamed range: the copy with the same
    /// offset into the range.
    pub(crate) fn resolve(&self, addr: u64, size: u32) -> Option<u64> {
        if self.entries.is_empty() {
            return None;
        }
        let i = self
            .entries
            .partition_point(|e| e.0 <= addr)
            .checked_sub(1)?;
        let (start, len, real) = self.entries[i];
        let inside = addr - start + u64::from(size) <= u64::from(len);
        inside.then(|| real + (addr - start))
    }
}
