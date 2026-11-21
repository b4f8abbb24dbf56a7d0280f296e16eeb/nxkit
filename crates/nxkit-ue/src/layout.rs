//! The slot layouts of Unreal's `FMalloc` vtable, told apart by the slot of the name getter (`GetDescriptiveName`),
//! which the rewriter reports (`UE_MALLOC_SLOT`). The layouts follow the old engine's `hle_uemalloc.c`, which measured
//! them from the vtables and their callers.

/// Which slots of the vtable this crate fills. A slot of 0 is a slot the layout does not have (it is never 0
/// otherwise: slots 0 and 1 are the destructors).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The name of the layout, for the log.
    pub name: &'static str,
    /// The slot of `GetDescriptiveName`.
    pub name_slot: u32,
    /// The words of the vtable that are copied.
    pub slots: usize,
    /// `Malloc(this, size, align)`.
    pub malloc: usize,
    /// `TryMalloc(this, size, align)`, or 0.
    pub try_malloc: usize,
    /// `Realloc(this, ptr, size, align)`.
    pub realloc: usize,
    /// `TryRealloc(this, ptr, size, align)`, or 0.
    pub try_realloc: usize,
    /// `Free(this, ptr)`.
    pub free: usize,
    /// `QuantizeSize(this, size, align)`.
    pub quantize: usize,
    /// `GetAllocationSize(this, ptr, &size) -> bool`.
    pub get_size: usize,
    /// The slots that do nothing here: `Trim`, the thread-local cache calls and `DumpAllocatorStats`. The other
    /// slots keep the guest's own functions.
    pub nothing: &'static [usize],
}

/// The layouts this crate knows.
pub const LAYOUTS: [Layout; 4] = [
    // Unreal Engine 5 (Octopath Traveler 0): two destructors, four FExec slots, then the allocator.
    Layout {
        name: "Unreal Engine 5",
        name_slot: 22,
        slots: 26,
        malloc: 6,
        try_malloc: 7,
        realloc: 8,
        try_realloc: 9,
        free: 10,
        quantize: 11,
        get_size: 12,
        nothing: &[13, 14, 15, 19],
    },
    // Unreal Engine 4.26 and 4.27 (OCTOPATH TRAVELER II, Pikmin 4): one FExec slot, no fork callbacks. OCTOPATH
    // TRAVELER II has no OnMallocInitialized, so the 21st word is the next vtable's offset to top, which nothing calls.
    Layout {
        name: "Unreal Engine 4.26/4.27",
        name_slot: 19,
        slots: 21,
        malloc: 3,
        try_malloc: 4,
        realloc: 5,
        try_realloc: 6,
        free: 7,
        quantize: 8,
        get_size: 9,
        nothing: &[10, 11, 12, 16],
    },
    // Unreal Engine 5.6 (FINAL FANTASY RESONANCE): MallocZeroed and TryMallocZeroed after Free keep the guest's
    // functions, which call Malloc through the vtable.
    Layout {
        name: "Unreal Engine 5.6",
        name_slot: 26,
        slots: 32,
        malloc: 6,
        try_malloc: 7,
        realloc: 8,
        try_realloc: 9,
        free: 10,
        quantize: 13,
        get_size: 14,
        nothing: &[15, 16, 17, 18, 19, 23],
    },
    // Unreal Engine 4.24 (DRAGON BALL Z: KAKAROT): 18 slots, no TryMalloc or TryRealloc.
    Layout {
        name: "Unreal Engine 4.24",
        name_slot: 17,
        slots: 18,
        malloc: 3,
        try_malloc: 0,
        realloc: 4,
        try_realloc: 0,
        free: 5,
        quantize: 6,
        get_size: 7,
        nothing: &[8, 9, 10, 14],
    },
];

/// The layout whose name getter sits at `slot`.
pub fn layout_for_name_slot(slot: u32) -> Option<&'static Layout> {
    LAYOUTS.iter().find(|l| l.name_slot == slot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_layout_fills_distinct_slots_inside_its_vtable() {
        for l in &LAYOUTS {
            let mut used: Vec<usize> = [
                l.malloc,
                l.try_malloc,
                l.realloc,
                l.try_realloc,
                l.free,
                l.quantize,
                l.get_size,
            ]
            .into_iter()
            .filter(|&s| s != 0)
            .chain(l.nothing.iter().copied())
            .collect();
            let n = used.len();
            used.sort_unstable();
            used.dedup();
            assert_eq!(used.len(), n, "{}: a slot twice", l.name);
            assert!(used.iter().all(|&s| s >= 2 && s < l.slots), "{}", l.name);
            assert!((l.name_slot as usize) < l.slots && !used.contains(&(l.name_slot as usize)));
        }
        assert_eq!(layout_for_name_slot(0x13).map(|l| l.malloc), Some(3));
        assert!(layout_for_name_slot(5).is_none());
    }
}
