//! How much of its regions the game uses: the heap of `SetHeapSize`, the memory of `MapPhysicalMemory` and how far into
//! the alias region it reaches, and how far `MapMemory` reaches into the stack region.
//!
//! The no-copy GPU buffer covers the heap, stack and alias regions whole, and the GPU driver wires every page of it
//! while it is resident, used or not. The sizes of the regions come from the title (`memory` of `game.json`), and the
//! peaks here are what a title's numbers can shrink to. The host prints them (`nxkit-host` at the end of a run, and
//! `NXKIT_MEM_REPORT`).

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

static HEAP: AtomicU64 = AtomicU64::new(0);
static HEAP_PEAK: AtomicU64 = AtomicU64::new(0);
static PHYSICAL: AtomicU64 = AtomicU64::new(0);
static PHYSICAL_PEAK: AtomicU64 = AtomicU64::new(0);
static ALIAS_TOP: AtomicU64 = AtomicU64::new(0);
static STACK_TOP: AtomicU64 = AtomicU64::new(0);

/// The use of the regions, in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RegionUse {
    /// The heap size now.
    pub heap: u64,
    /// The largest heap size of the run.
    pub heap_peak: u64,
    /// The memory that `MapPhysicalMemory` maps now.
    pub physical: u64,
    /// The most of it at one time.
    pub physical_peak: u64,
    /// The highest end of a `MapPhysicalMemory` range, from the start of the alias region.
    pub alias_top: u64,
    /// The highest end of a `MapMemory` destination, from the start of the stack region.
    pub stack_top: u64,
}

/// The use so far.
pub fn get() -> RegionUse {
    RegionUse {
        heap: HEAP.load(Relaxed),
        heap_peak: HEAP_PEAK.load(Relaxed),
        physical: PHYSICAL.load(Relaxed),
        physical_peak: PHYSICAL_PEAK.load(Relaxed),
        alias_top: ALIAS_TOP.load(Relaxed),
        stack_top: STACK_TOP.load(Relaxed),
    }
}

pub(crate) fn heap(size: u64) {
    HEAP.store(size, Relaxed);
    HEAP_PEAK.fetch_max(size, Relaxed);
}

pub(crate) fn physical(used: u64) {
    PHYSICAL.store(used, Relaxed);
    PHYSICAL_PEAK.fetch_max(used, Relaxed);
}

pub(crate) fn alias_top(offset: u64) {
    ALIAS_TOP.fetch_max(offset, Relaxed);
}

pub(crate) fn stack_top(offset: u64) {
    STACK_TOP.fetch_max(offset, Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_peaks_stay_when_the_use_drops() {
        // the tests of the crate share the statics, so only lower bounds are checked
        heap(0x4000_0000);
        heap(0x1000);
        physical(0x2000_0000);
        physical(0);
        alias_top(0x3000_0000);
        stack_top(0x10_0000);
        let u = get();
        assert!(u.heap_peak >= 0x4000_0000);
        assert!(u.physical_peak >= 0x2000_0000);
        assert!(u.alias_top >= 0x3000_0000);
        assert!(u.stack_top >= 0x10_0000);
    }
}
