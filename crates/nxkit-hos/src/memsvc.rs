//! The memory svcs: `SetHeapSize`, `SetMemoryPermission`, `SetMemoryAttribute`, `MapMemory`, `UnmapMemory`,
//! `QueryMemory`, `MapPhysicalMemory` and `UnmapPhysicalMemory`.
//!
//! Each call checks its arguments against the kernel's block map (`space.rs`), changes the map, and then makes the
//! host's pages match through [`HostSpace`](crate::memory::HostSpace). The map lock covers one call. No call waits
//! while it holds it. Memory svcs are one family with no wait list, so the lock is shared state and not a global
//! lock (R7). Thread, lock and IPC paths never take it.
//!
//! The rules follow the engine (`dm/host/hle_svc.c`, svc 0x01 to 0x06, 0x2C and 0x2D) and the result codes of Horizon
//! (Atmosphere `svc_results.hpp`, checked on 2026-11-07). Where the two differ the code says which one it follows.

use crate::kernel::Kernel;
use crate::memory::{HostSpace, PAGE_SIZE};
use crate::result::{KResult, KResultOr};
use crate::space::{Block, attr, perm, state};
use crate::thread::lock;
use crate::vm::View;

/// `SetHeapSize` takes multiples of 2 MiB.
const HEAP_ALIGN: u64 = 0x20_0000;
/// The attribute bits that `SetMemoryAttribute` may change.
const CHANGEABLE_ATTR: u32 = attr::UNCACHED | attr::PERMISSION_LOCKED;

/// Checks that `addr` and `size` name whole pages and do not wrap. Returns the range.
pub(crate) fn page_range(addr: u64, size: u64) -> KResultOr<(u64, u64)> {
    if !addr.is_multiple_of(PAGE_SIZE) {
        return Err(KResult::INVALID_ADDRESS);
    }
    if size == 0 || !size.is_multiple_of(PAGE_SIZE) {
        return Err(KResult::INVALID_SIZE);
    }
    let end = addr
        .checked_add(size)
        .ok_or(KResult::INVALID_CURRENT_MEMORY)?;
    Ok((addr, end))
}

/// Whether `MapMemory` shares the pages: all three values are whole host pages, and the host may put other pages
/// under both ranges. Only `MapMemory` asks. The pinned set changes while the game runs (`crate::pinned`), so a later
/// answer can differ; `UnmapMemory` reads the choice that `MapMemory` recorded (`Space::views`).
fn shares(host: &dyn HostSpace, d0: u64, s0: u64, size: u64) -> bool {
    let page = host.page_size();
    [d0, s0, size].iter().all(|v| v.is_multiple_of(page))
        && host.can_remap(d0, size)
        && host.can_remap(s0, size)
}

fn is_free(b: &Block) -> bool {
    b.state == state::FREE
}

// Horizon FlagCanAlias is set for both modeled data states.
fn can_alias(state: u32) -> bool {
    matches!(state, state::NORMAL | state::CODE_DATA)
}

fn is_plain_normal(b: &Block) -> bool {
    b.state == state::NORMAL && b.attr & attr::LOCKED == 0 && b.perm == perm::RW
}

impl Kernel {
    pub(crate) fn host_failed(&self, what: &str) -> KResult {
        (self.config.log)(&format!("the host refused to change its pages for {what}"));
        KResult::OUT_OF_RESOURCE
    }

    /// `SetHeapSize`: the heap grows or shrinks to `size`. Returns the address of the heap. A shrink gives the pages
    /// back, and they read as zero if the heap grows over them again. A shrink over a page that is locked (the source
    /// of `MapMemory`, or transfer memory) is refused. Pages that a pin reaches (`crate::pinned`) stay where they are,
    /// zeroed and without access. The size counters are those of the guest's view and drop by the whole shrink.
    pub(crate) fn set_heap_size(&self, size: u64) -> KResultOr<u64> {
        let vm = self.vm()?;
        if !size.is_multiple_of(HEAP_ALIGN) {
            return Err(KResult::INVALID_SIZE);
        }
        if size > vm.layout.heap_cap {
            return Err(KResult::OUT_OF_MEMORY);
        }
        let mut sp = lock(&vm.space);
        let counted = vm.cfg.used_memory
            + if vm.cfg.map_limit {
                sp.physical_used
            } else {
                0
            };
        if size > vm.cfg.app_pool.saturating_sub(counted) {
            return Err(KResult::LIMIT_REACHED);
        }
        let base = vm.layout.heap_base;
        let old = sp.heap_size;
        if size > old {
            if !vm.cfg.host.protect(base + old, size - old, perm::RW) {
                return Err(self.host_failed("the heap growth"));
            }
            sp.map
                .set(base + old, base + size, state::NORMAL, perm::RW, 0, 0);
        } else if size < old {
            let (from, to) = (base + size, base + old);
            if !sp.map.all(from, to, |b| b.attr & attr::LOCKED == 0) {
                return Err(KResult::INVALID_CURRENT_MEMORY);
            }
            if !vm.cfg.host.release(from, to - from) {
                return Err(self.host_failed("the heap shrink"));
            }
            sp.map.set(from, to, state::FREE, perm::NONE, 0, 0);
        }
        sp.heap_size = size;
        crate::usage::heap(size);
        Ok(base)
    }

    /// `SetMemoryPermission`: the range must be normal or code-data memory that is not locked and has no permission
    /// lock. The new permission is none, read or read and write.
    pub(crate) fn set_memory_permission(&self, addr: u64, size: u64, new: u32) -> KResultOr<()> {
        let vm = self.vm()?;
        let (start, end) = page_range(addr, size)?;
        if !matches!(new, perm::NONE | perm::R | perm::RW) {
            return Err(KResult::INVALID_NEW_MEMORY_PERMISSION);
        }
        let mut sp = lock(&vm.space);
        let ok = |b: &Block| {
            matches!(b.state, state::NORMAL | state::CODE_DATA)
                && b.attr & (attr::LOCKED | attr::PERMISSION_LOCKED) == 0
        };
        if !sp.map.all(start, end, ok) {
            return Err(KResult::INVALID_CURRENT_MEMORY);
        }
        if !vm.cfg.host.protect(start, size, new) {
            return Err(self.host_failed("SetMemoryPermission"));
        }
        sp.map.rewrite(start, end, |b| Block { perm: new, ..*b });
        Ok(())
    }

    /// `SetMemoryAttribute`, after Atmosphere Mesosphere (`svc_memory.cpp` and `KPageTableBase::SetMemoryAttribute`,
    /// from memory and not checked). The mask holds only the uncached bit (8) and the permission lock bit (0x10),
    /// and `value` holds no bit outside the mask. The permission lock can be set and never cleared. A range with the
    /// uncached bit in the mask needs normal or code-data memory. A range with the lock bit in the mask needs code or
    /// code-data memory. Memory with the `LOCKED` attribute is refused. The host changes nothing for either bit.
    pub(crate) fn set_memory_attribute(
        &self,
        addr: u64,
        size: u64,
        mask: u32,
        value: u32,
    ) -> KResultOr<()> {
        let vm = self.vm()?;
        let (start, end) = page_range(addr, size)?;
        if mask & !CHANGEABLE_ATTR != 0
            || (mask | value) != mask
            || mask & attr::PERMISSION_LOCKED != value & attr::PERMISSION_LOCKED
        {
            return Err(KResult::INVALID_COMBINATION);
        }
        let mut sp = lock(&vm.space);
        let ok = |b: &Block| {
            let uncached =
                mask & attr::UNCACHED == 0 || matches!(b.state, state::NORMAL | state::CODE_DATA);
            let lockable = mask & attr::PERMISSION_LOCKED == 0
                || matches!(b.state, state::CODE | state::CODE_DATA);
            uncached && lockable && b.attr & attr::LOCKED == 0
        };
        if !sp.map.all(start, end, ok) {
            return Err(KResult::INVALID_CURRENT_MEMORY);
        }
        sp.map.rewrite(start, end, |b| Block {
            attr: b.attr & !mask | value,
            ..*b
        });
        Ok(())
    }

    /// `MapMemory`: `dst` in the stack region becomes a second view of `src`. The source is locked and has no access
    /// until `UnmapMemory`. The pages are shared when the host can share them (all three values are whole host pages).
    /// Otherwise the data is copied, and copied back by `UnmapMemory`. The choice is recorded with the mapping
    /// (`Space::views`), and `UnmapMemory` follows the record.
    pub(crate) fn map_memory(&self, dst: u64, src: u64, size: u64) -> KResultOr<()> {
        let vm = self.vm()?;
        let (d0, d1) = page_range(dst, size)?;
        let (s0, s1) = page_range(src, size)?;
        if d0 < s1 && s0 < d1 {
            return Err(KResult::INVALID_CURRENT_MEMORY);
        }
        if d0 < vm.layout.stack_base || d1 > vm.layout.stack_end {
            return Err(KResult::INVALID_MEMORY_REGION);
        }
        let mut sp = lock(&vm.space);
        let source_state = sp.map.query(s0).state;
        if !sp.map.all(d0, d1, is_free)
            || !sp.map.all(s0, s1, |b| {
                b.state == source_state && can_alias(b.state) && b.attr == 0 && b.perm == perm::RW
            })
        {
            return Err(KResult::INVALID_CURRENT_MEMORY);
        }
        let host = &vm.cfg.host;
        let shared = shares(host.as_ref(), d0, s0, size);
        let ok = if shared {
            host.remap(d0, s0, size, perm::RW) && host.protect(s0, size, perm::NONE)
        } else {
            host.protect(d0, size, perm::RW) && host.copy(d0, s0, size)
        };
        if !ok {
            return Err(self.host_failed("MapMemory"));
        }
        sp.views.insert(
            d0,
            View {
                src: s0,
                size,
                shared,
            },
        );
        sp.map.rewrite(s0, s1, |b| Block {
            perm: perm::NONE,
            attr: b.attr | attr::LOCKED,
            ..*b
        });
        sp.map.set(d0, d1, state::STACK, perm::RW, 0, s0);
        crate::usage::stack_top(d1 - vm.layout.stack_base);
        Ok(())
    }

    /// `UnmapMemory`: undoes one `MapMemory`. The three arguments must be those of the map.
    pub(crate) fn unmap_memory(&self, dst: u64, src: u64, size: u64) -> KResultOr<()> {
        let vm = self.vm()?;
        let (d0, d1) = page_range(dst, size)?;
        let (s0, s1) = page_range(src, size)?;
        let mut sp = lock(&vm.space);
        let b = sp.map.query(d0);
        let source_state = sp.map.query(s0).state;
        let locked = |b: &Block| {
            b.state == source_state
                && can_alias(b.state)
                && b.attr == attr::LOCKED
                && b.perm == perm::NONE
        };
        if b.start != d0 || b.end != d1 || b.state != state::STACK || b.src != s0 {
            return Err(KResult::INVALID_CURRENT_MEMORY);
        }
        if !sp.map.all(s0, s1, locked) {
            return Err(KResult::INVALID_CURRENT_MEMORY);
        }
        let host = &vm.cfg.host;
        // the choice of `MapMemory`, not a new one: shared pages are only unmapped at `dst` (`release` could zero them
        // in place, and that would zero the source), and copied pages are copied back
        let shared = match sp.views.get(&d0) {
            Some(v) if v.src == s0 && v.size == size => v.shared,
            _ => {
                (self.config.log)(&format!(
                    "UnmapMemory at {d0:#x}: no record of how MapMemory made it, the data is copied back"
                ));
                // copying is safe for pages that were shared too: the source gets access, and the bytes are copied
                // onto themselves
                if !host.protect(s0, size, perm::RW) {
                    return Err(self.host_failed("UnmapMemory"));
                }
                false
            }
        };
        let ok = if shared {
            host.protect(s0, size, perm::RW) && host.drop_view(d0, size)
        } else {
            host.copy(s0, d0, size) && host.release(d0, size)
        };
        if !ok {
            return Err(self.host_failed("UnmapMemory"));
        }
        sp.views.remove(&d0);
        sp.map.set(d0, d1, state::FREE, perm::NONE, 0, 0);
        sp.map.rewrite(s0, s1, |b| Block {
            perm: perm::RW,
            attr: b.attr & !attr::LOCKED,
            ..*b
        });
        Ok(())
    }

    /// `QueryMemory`: writes the 40-byte `MemoryInfo` of the block that holds `addr` to `info` and returns the page
    /// info, which is always zero. A `MemoryInfo` that cannot be written gives `InvalidPointer` (unverified against
    /// Horizon).
    pub(crate) fn query_memory(&self, info: u64, addr: u64) -> KResultOr<u64> {
        let vm = self.vm()?;
        let b = lock(&vm.space).map.query(addr);
        let m = &self.mem;
        let written = m.store64(info, b.start)
            && m.store64(info + 8, b.size())
            && m.store32(info + 16, b.state)
            && m.store32(info + 20, b.attr)
            && m.store32(info + 24, b.perm)
            && m.store32(info + 28, 0)
            && m.store32(info + 32, 0)
            && m.store32(info + 36, 0);
        if written {
            Ok(0)
        } else {
            Err(KResult::INVALID_POINTER)
        }
    }

    fn alias_range(&self, addr: u64, size: u64) -> KResultOr<(u64, u64)> {
        let vm = self.vm()?;
        let (start, end) = page_range(addr, size)?;
        if start < vm.layout.alias_base || end > vm.layout.alias_end {
            return Err(KResult::INVALID_MEMORY_REGION);
        }
        Ok((start, end))
    }

    /// `MapPhysicalMemory`: the free pages of the range in the alias region become normal read and write memory. With
    /// the map limit on, the pages count against the pool. Pages that are mapped already stay. The pages read as zero
    /// when they were never mapped or were given back.
    pub(crate) fn map_physical_memory(&self, addr: u64, size: u64) -> KResultOr<()> {
        let vm = self.vm()?;
        let (start, end) = self.alias_range(addr, size)?;
        let mut sp = lock(&vm.space);
        let usable = |b: &Block| b.state == state::FREE || is_plain_normal(b);
        if !sp.map.all(start, end, usable) {
            return Err(KResult::INVALID_CURRENT_MEMORY);
        }
        let free: Vec<Block> = sp
            .map
            .blocks(start, end)
            .into_iter()
            .filter(is_free)
            .collect();
        let new: u64 = free
            .iter()
            .map(|b| b.end.min(end) - b.start.max(start))
            .sum();
        if vm.cfg.map_limit {
            let used = vm.cfg.used_memory + sp.heap_size + sp.physical_used;
            if new > vm.cfg.app_pool.saturating_sub(used) {
                return Err(KResult::LIMIT_REACHED);
            }
        }
        for b in &free {
            let (s, e) = (b.start.max(start), b.end.min(end));
            if !vm.cfg.host.protect(s, e - s, perm::RW) {
                return Err(self.host_failed("MapPhysicalMemory"));
            }
            sp.map.set(s, e, state::NORMAL, perm::RW, 0, 0);
        }
        sp.physical_used += new;
        crate::usage::physical(sp.physical_used);
        crate::usage::alias_top(end - vm.layout.alias_base);
        Ok(())
    }

    /// `UnmapPhysicalMemory`: the mapped pages of the range in the alias region are given back. A locked page
    /// refuses the call. Pages that a pin reaches (`crate::pinned`) stay where they are, zeroed and without access;
    /// the kernel counts them as given back, and a later `MapPhysicalMemory` gives them access again and they read
    /// as zero.
    pub(crate) fn unmap_physical_memory(&self, addr: u64, size: u64) -> KResultOr<()> {
        let vm = self.vm()?;
        let (start, end) = self.alias_range(addr, size)?;
        let mut sp = lock(&vm.space);
        if !sp
            .map
            .all(start, end, |b| b.state == state::FREE || is_plain_normal(b))
        {
            return Err(KResult::INVALID_CURRENT_MEMORY);
        }
        let mapped: Vec<Block> = sp
            .map
            .blocks(start, end)
            .into_iter()
            .filter(|b| b.state == state::NORMAL)
            .collect();
        let mut freed = 0;
        for b in &mapped {
            let (s, e) = (b.start.max(start), b.end.min(end));
            if !vm.cfg.host.release(s, e - s) {
                return Err(self.host_failed("UnmapPhysicalMemory"));
            }
            sp.map.set(s, e, state::FREE, perm::NONE, 0, 0);
            freed += e - s;
        }
        sp.physical_used = sp.physical_used.saturating_sub(freed);
        crate::usage::physical(sp.physical_used);
        Ok(())
    }
}
