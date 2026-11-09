//! `HostSpace::release` of the host memory: giving pages back, except the pages that a pin keeps.

use super::{HostMemory, posix};
use crate::memory::HostSpace;

impl HostMemory {
    /// Gives `addr..addr + len` back. The whole host pages of the range are cut at the pinned host pages
    /// (`crate::pinned::split`): the pinned runs are zeroed in place and lose their access, because the GPU still
    /// uses those pages, and the rest is replaced with fresh pages. The parts that share a host page with memory that
    /// stays are zeroed. Without a pin in the range this is one `drop_view` for the whole pages.
    pub(super) fn release_range(&self, addr: u64, len: u64) -> bool {
        let ps = self.page_size();
        let first = addr.div_ceil(ps) * ps;
        let last = (addr + len) & !(ps - 1);
        if last <= first {
            return self.scrub(addr, len);
        }
        let runs = crate::pinned::split(first, last, ps);
        runs.into_iter().all(|(start, end, pinned)| {
            if pinned {
                self.scrub(start, end - start) && posix::protect(start, end - start, 0)
            } else {
                self.drop_view(start, end - start)
            }
        }) && self.release_edges(addr, len, first, last)
    }

    /// Zeros the parts of a released range that share a host page with memory that stays.
    fn release_edges(&self, addr: u64, len: u64, first: u64, last: u64) -> bool {
        (first == addr || self.scrub(addr, first - addr))
            && (last == addr + len || self.scrub(last, addr + len - last))
    }

    /// Zeros a range that shares a host page with memory that stays. The host page is made readable and writable.
    fn scrub(&self, addr: u64, len: u64) -> bool {
        let ps = self.page_size();
        let start = addr & !(ps - 1);
        let end = (addr + len).div_ceil(ps) * ps;
        posix::protect(start, end - start, 3) && posix::fill_zero(addr, len)
    }
}
