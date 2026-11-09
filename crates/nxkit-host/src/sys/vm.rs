//! What the system says about the memory of this process: `task_info(TASK_VM_INFO)`, a walk over the VM regions and
//! the malloc zones. The memory report of the host (`crate::memreport`) prints it.

use std::ffi::{c_int, c_void};

/// `TASK_VM_INFO`.
const TASK_VM_INFO: c_int = 22;
/// `VM_REGION_SUBMAP_INFO_COUNT_64`: the size of [`SubmapInfo`] in `int`s.
const SUBMAP_INFO_COUNT: u32 = 21;
const KERN_SUCCESS: c_int = 0;

/// `VM_MEMORY_IOKIT`.
const TAG_IOKIT: u32 = 21;
/// `VM_MEMORY_IOSURFACE`.
const TAG_IOSURFACE: u32 = 88;
/// `VM_MEMORY_IOACCELERATOR`: memory of the GPU driver that the process maps (shared buffers, command buffers).
const TAG_IOACCELERATOR: u32 = 100;

/// `struct task_vm_info` of `<mach/task_info.h>` up to rev 7 (93 `int`s), which is packed to 4 bytes.
#[repr(C, packed(4))]
#[derive(Clone, Copy)]
struct TaskVmInfo {
    virtual_size: u64,
    region_count: i32,
    page_size: i32,
    resident_size: u64,
    resident_size_peak: u64,
    device: u64,
    device_peak: u64,
    internal: u64,
    internal_peak: u64,
    external: u64,
    external_peak: u64,
    reusable: u64,
    reusable_peak: u64,
    purgeable_volatile_pmap: u64,
    purgeable_volatile_resident: u64,
    purgeable_volatile_virtual: u64,
    compressed: u64,
    compressed_peak: u64,
    compressed_lifetime: u64,
    phys_footprint: u64,
    min_address: u64,
    max_address: u64,
    ledger_phys_footprint_peak: i64,
    ledger_purgeable_nonvolatile: i64,
    ledger_purgeable_novolatile_compressed: i64,
    ledger_purgeable_volatile: i64,
    ledger_purgeable_volatile_compressed: i64,
    ledger_tag_network_nonvolatile: i64,
    ledger_tag_network_nonvolatile_compressed: i64,
    ledger_tag_network_volatile: i64,
    ledger_tag_network_volatile_compressed: i64,
    ledger_tag_media_footprint: i64,
    ledger_tag_media_footprint_compressed: i64,
    ledger_tag_media_nofootprint: i64,
    ledger_tag_media_nofootprint_compressed: i64,
    ledger_tag_graphics_footprint: i64,
    ledger_tag_graphics_footprint_compressed: i64,
    ledger_tag_graphics_nofootprint: i64,
    ledger_tag_graphics_nofootprint_compressed: i64,
    ledger_tag_neural_footprint: i64,
    ledger_tag_neural_footprint_compressed: i64,
    ledger_tag_neural_nofootprint: i64,
    ledger_tag_neural_nofootprint_compressed: i64,
    limit_bytes_remaining: u64,
    decompressions: i32,
    ledger_swapins: i64,
    ledger_tag_neural_nofootprint_total: i64,
    ledger_tag_neural_nofootprint_peak: i64,
}

/// `struct vm_region_submap_info_64` of `<mach/vm_region.h>`, which is packed to 4 bytes (84 bytes).
#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct SubmapInfo {
    protection: i32,
    max_protection: i32,
    inheritance: u32,
    offset: u64,
    user_tag: u32,
    pages_resident: u32,
    pages_shared_now_private: u32,
    pages_swapped_out: u32,
    pages_dirtied: u32,
    ref_count: u32,
    shadow_depth: u16,
    external_pager: u8,
    share_mode: u8,
    is_submap: i32,
    behavior: i32,
    object_id: u32,
    user_wired_count: u16,
    flags: u16,
    pages_reusable: u32,
    object_id_full: u64,
    pages_wired: u32,
    wire_tag: u16,
    padding0: u16,
}

const _: () = assert!(std::mem::size_of::<SubmapInfo>() == 4 * SUBMAP_INFO_COUNT as usize);
const _: () = assert!(std::mem::size_of::<TaskVmInfo>() == 372);

/// `malloc_statistics_t` of `<malloc/malloc.h>`.
#[repr(C)]
#[derive(Default)]
struct MallocStats {
    blocks_in_use: u32,
    size_in_use: usize,
    max_size_in_use: usize,
    size_allocated: usize,
}

unsafe extern "C" {
    static mach_task_self_: u32;
    fn task_info(task: u32, flavor: c_int, info: *mut c_int, count: *mut u32) -> c_int;
    fn mach_vm_region_recurse(
        task: u32,
        address: *mut u64,
        size: *mut u64,
        depth: *mut u32,
        info: *mut c_int,
        count: *mut u32,
    ) -> c_int;
    fn mach_vm_page_range_query(
        task: u32,
        address: u64,
        size: u64,
        dispositions: u64,
        count: *mut u64,
    ) -> c_int;
    fn malloc_zone_statistics(zone: *mut c_void, stats: *mut MallocStats);
}

/// The counters of `TASK_VM_INFO`, in bytes.
#[derive(Clone, Copy, Debug, Default)]
pub struct TaskMemory {
    /// What Activity Monitor and jetsam count (`phys_footprint`).
    pub footprint: u64,
    /// The peak of `footprint`.
    pub footprint_peak: u64,
    /// Resident pages mapped in the process.
    pub resident: u64,
    /// Anonymous memory that is resident (`internal`).
    pub internal: u64,
    /// File-backed memory that is resident (`external`): mapped files, the pipeline archive, metallibs.
    pub external: u64,
    /// Anonymous memory in the compressor or in swap.
    pub compressed: u64,
    /// The memory of the GPU driver that counts in the footprint (`ledger_tag_graphics_footprint`): private textures,
    /// buffers, pipeline code, as the driver tags it.
    pub graphics: u64,
    /// The part of `graphics` that sits in the compressor.
    pub graphics_compressed: u64,
}

/// Reads `TASK_VM_INFO`. None when the call fails.
pub fn task_memory() -> Option<TaskMemory> {
    // SAFETY: an all-zero `TaskVmInfo` is valid, it holds only integers.
    let mut info: TaskVmInfo = unsafe { std::mem::zeroed() };
    let mut count = (std::mem::size_of::<TaskVmInfo>() / 4) as u32;
    // SAFETY: `info` is writable for `count` ints, which is the size the kernel may fill. `mach_task_self_` is the
    // port of this task, set by the system before `main`.
    let kr = unsafe {
        task_info(
            mach_task_self_,
            TASK_VM_INFO,
            (&raw mut info).cast::<c_int>(),
            &mut count,
        )
    };
    if kr != KERN_SUCCESS {
        return None;
    }
    let pos = |v: i64| u64::try_from(v).unwrap_or(0);
    Some(TaskMemory {
        footprint: info.phys_footprint,
        footprint_peak: pos(info.ledger_phys_footprint_peak),
        resident: info.resident_size,
        internal: info.internal,
        external: info.external,
        compressed: info.compressed,
        graphics: pos(info.ledger_tag_graphics_footprint),
        graphics_compressed: pos(info.ledger_tag_graphics_footprint_compressed),
    })
}

/// The pages of one kind of region, in bytes.
#[derive(Clone, Copy, Debug, Default)]
pub struct Pages {
    /// Mapped address space.
    pub mapped: u64,
    /// Resident in RAM.
    pub resident: u64,
    /// Written (resident and dirty).
    pub dirty: u64,
    /// In the compressor or in swap.
    pub swapped: u64,
}

impl Pages {
    fn add(&mut self, size: u64, i: &SubmapInfo, page: u64) {
        self.mapped += size;
        self.resident += u64::from(i.pages_resident) * page;
        self.dirty += u64::from(i.pages_dirtied) * page;
        self.swapped += u64::from(i.pages_swapped_out) * page;
    }
}

/// The VM regions of the process by kind.
#[derive(Clone, Copy, Debug, Default)]
pub struct Regions {
    /// Inside the guest range that the caller named.
    pub guest: Pages,
    /// Tagged `VM_MEMORY_MALLOC*` (the host's heap: Rust allocations, video pictures, Objective-C objects).
    pub malloc: Pages,
    /// Tagged `VM_MEMORY_IOACCELERATOR` (GPU driver memory that the process maps, mostly shared buffers).
    pub ioaccel: Pages,
    /// Tagged `VM_MEMORY_IOSURFACE` (drawables, video pictures of VideoToolbox).
    pub iosurface: Pages,
    /// Tagged `VM_MEMORY_IOKIT`.
    pub iokit: Pages,
    /// Mapped from a file (an external pager): the pipeline archive, metallibs, dylibs.
    pub files: Pages,
    /// Everything else (thread stacks, untagged anonymous memory).
    pub other: Pages,
}

/// Walks every VM region of the process. The guest range is `[guest_start, guest_end)`; a region that lies across its
/// edge counts by its start. Expensive (the kernel visits the pages of each region): call it every few seconds at most.
pub fn regions(guest_start: u64, guest_end: u64, page: u64) -> Regions {
    let mut out = Regions::default();
    let mut addr: u64 = 0;
    let mut depth: u32 = 0;
    loop {
        let mut size: u64 = 0;
        let mut info = SubmapInfo::default();
        let mut count = SUBMAP_INFO_COUNT;
        // SAFETY: `info` is writable for `count` ints; the other pointers are to locals. The call only reads the
        // map of this task.
        let kr = unsafe {
            mach_vm_region_recurse(
                mach_task_self_,
                &mut addr,
                &mut size,
                &mut depth,
                (&raw mut info).cast::<c_int>(),
                &mut count,
            )
        };
        if kr != KERN_SUCCESS || size == 0 {
            break;
        }
        if info.is_submap != 0 {
            depth += 1;
            continue;
        }
        let tag = info.user_tag;
        let bucket = if addr >= guest_start && addr < guest_end {
            &mut out.guest
        } else if (1..=12).contains(&tag) {
            &mut out.malloc
        } else if tag == TAG_IOACCELERATOR {
            &mut out.ioaccel
        } else if tag == TAG_IOSURFACE {
            &mut out.iosurface
        } else if tag == TAG_IOKIT {
            &mut out.iokit
        } else if info.external_pager != 0 {
            &mut out.files
        } else {
            &mut out.other
        };
        bucket.add(size, &info, page);
        match addr.checked_add(size) {
            Some(next) => addr = next,
            None => break,
        }
    }
    out
}

/// The bytes in use in all malloc zones.
pub fn malloc_in_use() -> u64 {
    let mut s = MallocStats::default();
    // SAFETY: a null zone asks for the sum of all zones; `s` is a writable `malloc_statistics_t`.
    unsafe { malloc_zone_statistics(std::ptr::null_mut(), &mut s) };
    s.size_in_use as u64
}

/// `VM_PAGE_QUERY_PAGE_PRESENT`.
const PAGE_PRESENT: i32 = 0x1;
/// `VM_PAGE_QUERY_PAGE_DIRTY`.
const PAGE_DIRTY: i32 = 0x4;
/// `VM_PAGE_QUERY_PAGE_PAGED_OUT`.
const PAGE_PAGED_OUT: i32 = 0x8;

/// Counts the pages of one query by their disposition bits.
fn count_pages(dispositions: &[i32], page: u64) -> Pages {
    let mut p = Pages::default();
    for &d in dispositions {
        if d & PAGE_PRESENT != 0 {
            p.resident += page;
        }
        if d & PAGE_DIRTY != 0 {
            p.dirty += page;
        }
        if d & PAGE_PAGED_OUT != 0 {
            p.swapped += page;
        }
    }
    p
}

/// The pages of `[start, end)` that the kernel has resident, dirty or paged out, from a query of each page. The region
/// walk of [`regions`] counts by VM object, and for the guest range (one anonymous mapping under a no-copy Metal buffer)
/// it reported 12.6 GiB resident next to a footprint of 113 MiB. The per-page query is the ground truth. None when the
/// kernel refuses the query. `mapped` is the length of the range.
pub fn page_query(start: u64, end: u64, page: u64) -> Option<Pages> {
    const CHUNK: u64 = 1 << 30;
    let mut out = Pages {
        mapped: end.saturating_sub(start),
        ..Pages::default()
    };
    let mut buf: Vec<i32> = vec![0; (CHUNK / page.max(1)) as usize];
    let mut at = start;
    while at < end {
        let len = (end - at).min(CHUNK);
        let mut count = len / page;
        if count == 0 {
            break;
        }
        // SAFETY: `buf` holds `CHUNK / page` ints and `count` is at most that. The kernel writes one int per page of
        // `[at, at + len)` and updates `count`. The call only reads the map of this task.
        let kr = unsafe {
            mach_vm_page_range_query(
                mach_task_self_,
                at,
                len,
                buf.as_mut_ptr() as u64,
                &mut count,
            )
        };
        if kr != KERN_SUCCESS {
            return None;
        }
        let n = (count as usize).min(buf.len());
        let c = count_pages(&buf[..n], page);
        out.resident += c.resident;
        out.dirty += c.dirty;
        out.swapped += c.swapped;
        at += len;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_are_counted_by_bit() {
        let d = [
            0,
            PAGE_PRESENT,
            PAGE_PRESENT | PAGE_DIRTY,
            PAGE_PAGED_OUT,
            PAGE_PRESENT | PAGE_DIRTY | PAGE_PAGED_OUT,
        ];
        let p = count_pages(&d, 16384);
        assert_eq!(p.resident, 3 * 16384);
        assert_eq!(p.dirty, 2 * 16384);
        assert_eq!(p.swapped, 2 * 16384);
    }
}
