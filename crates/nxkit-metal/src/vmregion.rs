//! The VM region that holds an address of this process, for the diagnostic that asks whether two guest addresses are
//! two views of the same pages (`NXKIT_DIAG=ndvetrace`).
//!
//! Two views made by `mach_vm_remap` (the guest's `MapMemory` and `MapSharedMemory`) share one VM object: the same
//! object id and, for two addresses that show the same byte, the same offset into that object.

use std::ffi::c_int;

/// `VM_REGION_SUBMAP_INFO_COUNT_64`: the size of [`SubmapInfo`] in `int`s.
const SUBMAP_INFO_COUNT: u32 = 21;
const KERN_SUCCESS: c_int = 0;
/// The submaps that are followed at most.
const MAX_DEPTH: u32 = 8;

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

unsafe extern "C" {
    static mach_task_self_: u32;
    fn mach_vm_region_recurse(
        task: u32,
        address: *mut u64,
        size: *mut u64,
        depth: *mut u32,
        info: *mut c_int,
        count: *mut u32,
    ) -> c_int;
}

/// The VM region that holds an address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VmRegion {
    /// The first address of the region.
    pub start: u64,
    /// The size in bytes.
    pub size: u64,
    /// The current protection (bit 0 read, 1 write, 2 execute).
    pub protection: u32,
    /// `SM_*` of `<mach/vm_region.h>`: 1 copy on write, 2 private, 4 shared, 5 true shared, 6 private aliased.
    pub share_mode: u8,
    /// The id of the VM object behind the region.
    pub object: u64,
    /// The offset into the object of the first byte of the region.
    pub object_offset: u64,
    /// The users of the object.
    pub ref_count: u32,
    /// `VM_MEMORY_*` tag.
    pub tag: u32,
}

impl VmRegion {
    /// The offset into the VM object of the byte at `addr`, which must lie in the region.
    pub fn offset_of(&self, addr: u64) -> u64 {
        self.object_offset + addr.saturating_sub(self.start)
    }

    /// True when the byte at `addr` here and the byte at `other_addr` in `other` are the same byte of one VM object.
    pub fn aliases(&self, addr: u64, other: &Self, other_addr: u64) -> bool {
        self.object == other.object && self.offset_of(addr) == other.offset_of(other_addr)
    }
}

/// The VM region that holds `addr`, or `None` when the address is not mapped.
pub fn vm_region(addr: u64) -> Option<VmRegion> {
    let mut at = addr;
    let mut depth = 0u32;
    loop {
        let mut size = 0u64;
        let mut info = SubmapInfo::default();
        let mut count = SUBMAP_INFO_COUNT;
        // SAFETY: `info` is writable for `count` ints, which is the size the kernel may fill; the other pointers are
        // to locals. The call only reads the map of this task, and `mach_task_self_` is its port.
        let kr = unsafe {
            mach_vm_region_recurse(
                mach_task_self_,
                &mut at,
                &mut size,
                &mut depth,
                (&raw mut info).cast::<c_int>(),
                &mut count,
            )
        };
        if kr != KERN_SUCCESS || size == 0 {
            return None;
        }
        if info.is_submap != 0 && depth < MAX_DEPTH {
            depth += 1;
            continue;
        }
        if at > addr {
            return None;
        }
        return Some(VmRegion {
            start: at,
            size,
            protection: info.protection as u32,
            share_mode: info.share_mode,
            object: info.object_id_full,
            object_offset: info.offset,
            ref_count: info.ref_count,
            tag: info.user_tag,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_variable_lies_in_a_region_and_is_its_own_alias() {
        let x = 5u64;
        let a = &raw const x as u64;
        let r = vm_region(a);
        assert!(r.is_some_and(|r| r.start <= a && a < r.start + r.size));
        assert!(r.is_some_and(|r| r.aliases(a, &r, a)));
    }
}
