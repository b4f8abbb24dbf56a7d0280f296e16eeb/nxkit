//! The system interface of the tracker: signal and Mach declarations and their structures.

use super::*;

/// `struct sigaction` of `<sys/signal.h>`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct SigAction {
    pub(super) handler: usize,
    pub(super) mask: u32,
    pub(super) flags: c_int,
}

/// `siginfo_t`, up to `si_addr`.
#[repr(C)]
pub(super) struct SigInfo {
    signo: c_int,
    errno: c_int,
    code: c_int,
    pid: c_int,
    uid: u32,
    status: c_int,
    pub(super) addr: usize,
}

pub(super) type Handler = extern "C" fn(c_int, *mut SigInfo, *mut c_void);

pub(super) const MAP_PRIVATE: c_int = 0x2;
pub(super) const MAP_ANON: c_int = 0x1000;
pub(super) const MAP_NORESERVE: c_int = 0x40;
pub(super) const VM_FLAGS_FIXED: c_int = 0;
pub(super) const VM_FLAGS_OVERWRITE: c_int = 0x4000;
pub(super) const VM_INHERIT_NONE: c_int = 2;

unsafe extern "C" {
    pub(super) static mach_task_self_: u32;
    pub(super) fn mach_vm_remap(
        target_task: u32,
        target_address: *mut u64,
        size: u64,
        mask: u64,
        flags: c_int,
        src_task: u32,
        src_address: u64,
        copy: c_int,
        cur_protection: *mut c_int,
        max_protection: *mut c_int,
        inheritance: c_int,
    ) -> c_int;
    pub(super) fn mach_vm_protect(
        task: u32,
        address: u64,
        size: u64,
        set_maximum: c_int,
        prot: c_int,
    ) -> c_int;
    pub(super) fn mmap(
        addr: *mut c_void,
        len: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        off: i64,
    ) -> *mut c_void;
    pub(super) fn munmap(addr: *mut c_void, len: usize) -> c_int;
    pub(super) fn mprotect(addr: *mut c_void, len: usize, prot: c_int) -> c_int;
    pub(super) fn getpagesize() -> c_int;
    pub(super) fn sigaction(sig: c_int, act: *const SigAction, old: *mut SigAction) -> c_int;
    pub(super) fn pthread_self() -> usize;
    pub(super) fn clock_gettime_nsec_np(clock: u32) -> u64;
    pub(super) fn os_sync_wait_on_address_with_timeout(
        addr: *mut c_void,
        value: u64,
        size: usize,
        flags: u32,
        clockid: u32,
        timeout_ns: u64,
    ) -> c_int;
    pub(super) fn os_sync_wake_by_address_all(addr: *mut c_void, size: usize, flags: u32) -> c_int;
    pub(super) fn dladdr(addr: *const c_void, info: *mut DlInfo) -> c_int;
}

/// `Dl_info`.
#[repr(C)]
pub(super) struct DlInfo {
    pub(super) fname: *const std::ffi::c_char,
    pub(super) fbase: usize,
    pub(super) sname: *const std::ffi::c_char,
    pub(super) saddr: usize,
}
