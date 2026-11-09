//! The paging counters of the process and of the system, for the rates of the memory report (`crate::memreport`):
//! `task_info(TASK_EVENTS_INFO)` for the faults and page-ins of this process, `host_statistics64(HOST_VM_INFO64)` for
//! the system's compressor and swap, and `kern.memorystatus_level`, the percentage of memory the system counts as free.

use std::ffi::{CStr, c_char, c_int, c_void};

/// `TASK_EVENTS_INFO`.
const TASK_EVENTS_INFO: c_int = 2;
/// `HOST_VM_INFO64`.
const HOST_VM_INFO64: c_int = 4;
const KERN_SUCCESS: c_int = 0;

/// `struct task_events_info` of `<mach/task_info.h>`: eight `integer_t`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct TaskEventsInfo {
    faults: i32,
    pageins: i32,
    cow_faults: i32,
    messages_sent: i32,
    messages_received: i32,
    syscalls_mach: i32,
    syscalls_unix: i32,
    csw: i32,
}

/// `struct vm_statistics64` of `<mach/vm_statistics.h>`, up to `swapped_count` (`HOST_VM_INFO64_REV2_COUNT`, 40 `int`s).
#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct VmStatistics64 {
    free_count: u32,
    active_count: u32,
    inactive_count: u32,
    wire_count: u32,
    zero_fill_count: u64,
    reactivations: u64,
    pageins: u64,
    pageouts: u64,
    faults: u64,
    cow_faults: u64,
    lookups: u64,
    hits: u64,
    purges: u64,
    purgeable_count: u32,
    speculative_count: u32,
    decompressions: u64,
    compressions: u64,
    swapins: u64,
    swapouts: u64,
    compressor_page_count: u32,
    throttled_count: u32,
    external_page_count: u32,
    internal_page_count: u32,
    total_uncompressed_pages_in_compressor: u64,
    swapped_count: u64,
}

const _: () = assert!(std::mem::size_of::<VmStatistics64>() == 4 * 40);
const _: () = assert!(std::mem::size_of::<TaskEventsInfo>() == 4 * 8);

unsafe extern "C" {
    static mach_task_self_: u32;
    fn task_info(task: u32, flavor: c_int, info: *mut c_int, count: *mut u32) -> c_int;
    fn mach_host_self() -> u32;
    fn host_statistics64(host: u32, flavor: c_int, info: *mut c_int, count: *mut u32) -> c_int;
    fn sysctlbyname(
        name: *const c_char,
        old: *mut c_void,
        oldlen: *mut usize,
        new: *mut c_void,
        newlen: usize,
    ) -> c_int;
}

/// The fault counters of this process since its start (`TASK_EVENTS_INFO`). The kernel keeps them as 32 bits, so they
/// wrap; [`per_second`] takes the difference with wrapping.
#[derive(Clone, Copy, Debug, Default)]
pub struct TaskEvents {
    /// Page faults of every kind.
    pub faults: u32,
    /// Faults that read a page from a pager: a file, the compressor or swap.
    pub pageins: u32,
}

/// Reads `TASK_EVENTS_INFO`. None when the call fails.
pub fn task_events() -> Option<TaskEvents> {
    let mut info = TaskEventsInfo::default();
    let mut count = (std::mem::size_of::<TaskEventsInfo>() / 4) as u32;
    // SAFETY: `info` is writable for `count` ints. `mach_task_self_` is the port of this task.
    let kr = unsafe {
        task_info(
            mach_task_self_,
            TASK_EVENTS_INFO,
            (&raw mut info).cast::<c_int>(),
            &mut count,
        )
    };
    (kr == KERN_SUCCESS).then_some(TaskEvents {
        faults: info.faults as u32,
        pageins: info.pageins as u32,
    })
}

/// The system's memory: pages now in bytes, events since boot as counts of pages.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemVm {
    /// Free memory in bytes.
    pub free: u64,
    /// Wired memory in bytes. The no-copy guest buffer is in it while it is resident.
    pub wired: u64,
    /// The memory that the compressor occupies, in bytes.
    pub compressor: u64,
    /// Pages taken out of the compressor.
    pub decompressions: u64,
    /// Pages put into the compressor.
    pub compressions: u64,
    /// Pages read from a pager.
    pub pageins: u64,
    /// Pages read back from swap.
    pub swapins: u64,
    /// Pages written to swap.
    pub swapouts: u64,
    /// `kern.memorystatus_level`: the percentage of memory the system counts as available (what `memory_pressure`
    /// prints as "free percentage"). None when the sysctl fails.
    pub level: Option<u32>,
}

/// Reads `HOST_VM_INFO64` and the memory level. None when the call fails. `page` is the page size in bytes.
pub fn system_vm(page: u64) -> Option<SystemVm> {
    let mut v = VmStatistics64::default();
    let mut count = (std::mem::size_of::<VmStatistics64>() / 4) as u32;
    // SAFETY: `v` is writable for `count` ints. `mach_host_self` returns a send right to the host port; it is not
    // released, which leaks one reference per call, as every caller of the function in the SDK does.
    let kr = unsafe {
        host_statistics64(
            mach_host_self(),
            HOST_VM_INFO64,
            (&raw mut v).cast::<c_int>(),
            &mut count,
        )
    };
    if kr != KERN_SUCCESS {
        return None;
    }
    Some(SystemVm {
        free: u64::from(v.free_count) * page,
        wired: u64::from(v.wire_count) * page,
        compressor: u64::from(v.compressor_page_count) * page,
        decompressions: v.decompressions,
        compressions: v.compressions,
        pageins: v.pageins,
        swapins: v.swapins,
        swapouts: v.swapouts,
        level: sysctl_u32(c"kern.memorystatus_level"),
    })
}

fn sysctl_u32(name: &CStr) -> Option<u32> {
    let mut v: u32 = 0;
    let mut len = std::mem::size_of::<u32>();
    // SAFETY: `name` is a C string; `v` is writable for `len` bytes. The call writes at most `len` bytes.
    let rc = unsafe {
        sysctlbyname(
            name.as_ptr(),
            (&raw mut v).cast::<c_void>(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0 && len == std::mem::size_of::<u32>()).then_some(v)
}

/// The rate of a counter between two samples, per second. A counter that went backwards counts as having wrapped
/// at `wrap` (32 bits for the task counters); with `wrap` 0 it counts as 0.
pub fn per_second(before: u64, now: u64, wrap: u64, secs: f64) -> u64 {
    let d = if now >= before {
        now - before
    } else if wrap > before {
        wrap - before + now
    } else {
        0
    };
    if secs <= 0.0 {
        return 0;
    }
    (d as f64 / secs).round() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rate_is_the_difference_over_the_time() {
        assert_eq!(per_second(100, 1100, 0, 2.0), 500);
        assert_eq!(per_second(5, 5, 0, 1.0), 0);
        assert_eq!(per_second(10, 20, 0, 0.0), 0);
    }

    #[test]
    fn a_task_counter_wraps_at_32_bits() {
        let wrap = 1u64 << 32;
        assert_eq!(per_second(wrap - 10, 5, wrap, 1.0), 15);
        assert_eq!(per_second(50, 10, 0, 1.0), 0);
    }

    #[test]
    fn the_counters_can_be_read() {
        let t = task_events();
        assert!(t.is_some_and(|t| t.faults > 0));
        let s = system_vm(16384);
        assert!(s.is_some_and(|s| s.free > 0 && s.wired > 0));
    }
}
