//! The only module with `unsafe`: address waits, thread QoS, raw guest memory and calls into SDK functions (the
//! child module `call`).
//!
//! On Apple platforms the waits are `os_sync_wait_on_address` and `os_sync_wake_by_address_*` (macOS 14.4, iOS 17.4).
//! Other platforms get a safe fallback of sharded mutexes and condition variables, so the crate builds and tests there.

#![allow(unsafe_code)]

use crate::memory::{GuestMemory, HostSpace, PAGE_SIZE};
use crate::role::Qos;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

mod call;

pub use call::{GuestFn, random_fill};
pub(crate) use call::{drop_spawn_arg, run_spawned, spawn_arg};

/// Why a wait returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitStatus {
    /// Woken, or spurious. The caller checks its word again.
    Woken,
    /// The timeout ran out.
    TimedOut,
}

#[cfg(target_vendor = "apple")]
mod apple {
    use std::ffi::{c_int, c_void};

    pub(super) const OS_CLOCK_MACH_ABSOLUTE_TIME: u32 = 32;
    pub(super) const ETIMEDOUT: i32 = 60;

    // SAFETY (all declarations): these are system functions with the signatures of `<os/os_sync_wait_on_address.h>`
    // and `<pthread/qos.h>`. Both headers are in the SDK.
    unsafe extern "C" {
        pub(super) fn os_sync_wait_on_address(
            addr: *mut c_void,
            value: u64,
            size: usize,
            flags: u32,
        ) -> c_int;
        pub(super) fn os_sync_wait_on_address_with_timeout(
            addr: *mut c_void,
            value: u64,
            size: usize,
            flags: u32,
            clockid: u32,
            timeout_ns: u64,
        ) -> c_int;
        pub(super) fn os_sync_wake_by_address_any(
            addr: *mut c_void,
            size: usize,
            flags: u32,
        ) -> c_int;
        pub(super) fn os_sync_wake_by_address_all(
            addr: *mut c_void,
            size: usize,
            flags: u32,
        ) -> c_int;
        pub(super) fn pthread_set_qos_class_self_np(qos: u32, relative_priority: c_int) -> c_int;
        pub(super) fn pthread_get_qos_class_np(
            thread: *mut c_void,
            qos: *mut u32,
            relative_priority: *mut c_int,
        ) -> c_int;
        pub(super) fn pthread_self() -> *mut c_void;
        pub(super) fn pthread_override_qos_class_start_np(
            thread: *mut c_void,
            qos: u32,
            relative_priority: c_int,
        ) -> *mut c_void;
        pub(super) fn pthread_override_qos_class_end_np(over: *mut c_void) -> c_int;
    }
}

#[cfg(target_vendor = "apple")]
fn wait_apple(word: &AtomicU32, expected: u32, timeout: Option<Duration>) -> WaitStatus {
    let addr = word as *const AtomicU32 as *mut std::ffi::c_void;
    let size = std::mem::size_of::<u32>();
    let ret = match timeout {
        // SAFETY: `addr` points to a live, aligned `AtomicU32` borrowed for the call. The kernel only reads 4 bytes.
        None => unsafe { apple::os_sync_wait_on_address(addr, u64::from(expected), size, 0) },
        Some(d) => {
            let ns = u64::try_from(d.as_nanos()).unwrap_or(u64::MAX).max(1);
            // SAFETY: as above.
            unsafe {
                apple::os_sync_wait_on_address_with_timeout(
                    addr,
                    u64::from(expected),
                    size,
                    0,
                    apple::OS_CLOCK_MACH_ABSOLUTE_TIME,
                    ns,
                )
            }
        }
    };
    if ret < 0 && std::io::Error::last_os_error().raw_os_error() == Some(apple::ETIMEDOUT) {
        WaitStatus::TimedOut
    } else {
        WaitStatus::Woken
    }
}

#[cfg(not(target_vendor = "apple"))]
mod fallback {
    use std::sync::{Condvar, Mutex};

    pub(super) struct Slot {
        pub(super) lock: Mutex<()>,
        pub(super) cv: Condvar,
    }
    #[allow(clippy::declare_interior_mutable_const)]
    const SLOT: Slot = Slot {
        lock: Mutex::new(()),
        cv: Condvar::new(),
    };
    pub(super) static SLOTS: [Slot; 64] = [SLOT; 64];

    pub(super) fn slot(addr: usize) -> &'static Slot {
        &SLOTS[(addr >> 2).wrapping_mul(0x9E37_79B1) >> 8 & 63]
    }
}

/// Blocks while `*word == expected`, until woken or until `timeout` ends. `None` waits without a limit.
pub fn wait_u32(word: &AtomicU32, expected: u32, timeout: Option<Duration>) -> WaitStatus {
    #[cfg(target_vendor = "apple")]
    {
        if word.load(Ordering::Acquire) != expected {
            return WaitStatus::Woken;
        }
        wait_apple(word, expected, timeout)
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let slot = fallback::slot(word as *const AtomicU32 as usize);
        let guard = slot.lock.lock().unwrap_or_else(|e| e.into_inner());
        if word.load(Ordering::Acquire) != expected {
            return WaitStatus::Woken;
        }
        match timeout {
            None => {
                drop(slot.cv.wait(guard));
                WaitStatus::Woken
            }
            Some(d) => {
                let (g, r) = slot
                    .cv
                    .wait_timeout(guard, d)
                    .unwrap_or_else(|e| e.into_inner());
                drop(g);
                if r.timed_out() {
                    WaitStatus::TimedOut
                } else {
                    WaitStatus::Woken
                }
            }
        }
    }
}

/// Wakes one thread that waits on `word`. A waiter may also wake without a call, so callers recheck.
pub fn wake_one(word: &AtomicU32) {
    #[cfg(target_vendor = "apple")]
    {
        // SAFETY: `word` is a live `AtomicU32`. The call only looks the address up in the kernel's wait table.
        unsafe {
            apple::os_sync_wake_by_address_any(
                word as *const AtomicU32 as *mut std::ffi::c_void,
                std::mem::size_of::<u32>(),
                0,
            );
        }
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let slot = fallback::slot(word as *const AtomicU32 as usize);
        let _g = slot.lock.lock().unwrap_or_else(|e| e.into_inner());
        slot.cv.notify_all();
    }
}

/// Wakes every thread that waits on `word`.
pub fn wake_all(word: &AtomicU32) {
    #[cfg(target_vendor = "apple")]
    {
        // SAFETY: as for `wake_one`.
        unsafe {
            apple::os_sync_wake_by_address_all(
                word as *const AtomicU32 as *mut std::ffi::c_void,
                std::mem::size_of::<u32>(),
                0,
            );
        }
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        wake_one(word);
    }
}

/// Sets the QoS class of the calling thread. Returns false when the platform has none or refuses it.
pub fn set_current_qos(qos: Qos) -> bool {
    #[cfg(target_vendor = "apple")]
    {
        // SAFETY: the call changes only the calling thread's scheduling class.
        unsafe { apple::pthread_set_qos_class_self_np(qos.raw(), 0) == 0 }
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let _ = qos;
        false
    }
}

/// The QoS class of the calling thread, when the platform reports one.
pub fn current_qos() -> Option<Qos> {
    #[cfg(target_vendor = "apple")]
    {
        let mut raw = 0u32;
        let mut rel = 0;
        // SAFETY: `raw` and `rel` are live locals. `pthread_self` is the calling thread.
        let rc =
            unsafe { apple::pthread_get_qos_class_np(apple::pthread_self(), &mut raw, &mut rel) };
        if rc == 0 { Qos::from_raw(raw) } else { None }
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        None
    }
}

/// The host id of the calling thread, for `QosOverride::start`. Zero when the platform has none.
pub fn current_host_thread() -> usize {
    #[cfg(target_vendor = "apple")]
    {
        // SAFETY: `pthread_self` has no preconditions.
        unsafe { apple::pthread_self() as usize }
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        0
    }
}

/// A QoS class override of another thread. The thread runs at least at that class until the value is dropped.
pub struct QosOverride(*mut std::ffi::c_void);

// SAFETY: the pointer is an opaque token that `pthread_override_qos_class_start_np` returned. It is never
// dereferenced here and only handed back to `pthread_override_qos_class_end_np`. Whether that call may come from
// another thread is not verified. The lend table (`lend.rs`) ends overrides on the thread that exits and on the
// waiter's thread, and the first one to run takes the value.
unsafe impl Send for QosOverride {}

impl QosOverride {
    /// Starts an override of the host thread `host` (from `current_host_thread`). `None` when the platform has no
    /// overrides or refuses it. The thread must still run, as the owner of a lock does.
    pub fn start(host: usize, qos: Qos) -> Option<QosOverride> {
        #[cfg(target_vendor = "apple")]
        {
            if host == 0 {
                return None;
            }
            // SAFETY: `host` came from `pthread_self` of a thread that holds a lock and so still runs. The call
            // allocates an override object that `Drop` hands back to `pthread_override_qos_class_end_np`.
            let over =
                unsafe { apple::pthread_override_qos_class_start_np(host as *mut _, qos.raw(), 0) };
            (!over.is_null()).then_some(QosOverride(over))
        }
        #[cfg(not(target_vendor = "apple"))]
        {
            let _ = (host, qos);
            None
        }
    }
}

impl Drop for QosOverride {
    fn drop(&mut self) {
        #[cfg(target_vendor = "apple")]
        {
            // SAFETY: the pointer is the live object that `start` returned. It is ended once, here.
            unsafe {
                apple::pthread_override_qos_class_end_np(self.0);
            }
        }
    }
}

/// Guest memory when the guest's addresses are host addresses, as in the nxkit host.
pub struct HostMemory(());

impl HostMemory {
    /// # Safety
    /// Every address the kernel receives from the guest must be mapped, readable and writable for the whole life of
    /// the value, or the access faults. The guest's own svc arguments are such addresses.
    pub unsafe fn new() -> HostMemory {
        HostMemory(())
    }

    fn word(addr: u64) -> Option<&'static AtomicU32> {
        if addr == 0 || !addr.is_multiple_of(4) {
            return None;
        }
        // SAFETY: `HostMemory::new` makes the caller promise that guest addresses are mapped and live. The address is
        // not null and is aligned for `AtomicU32`.
        Some(unsafe { &*(addr as *const AtomicU32) })
    }
}

impl GuestMemory for HostMemory {
    fn load32(&self, addr: u64) -> Option<u32> {
        Some(Self::word(addr)?.load(Ordering::SeqCst))
    }

    fn load64(&self, addr: u64) -> Option<u64> {
        if addr == 0 || !addr.is_multiple_of(8) {
            return None;
        }
        // SAFETY: as for `word`, with 8-byte alignment checked.
        Some(unsafe { &*(addr as *const std::sync::atomic::AtomicU64) }.load(Ordering::SeqCst))
    }

    fn store32(&self, addr: u64, value: u32) -> bool {
        Self::word(addr)
            .map(|w| w.store(value, Ordering::SeqCst))
            .is_some()
    }

    fn cas32(&self, addr: u64, current: u32, new: u32) -> Option<Result<u32, u32>> {
        Some(Self::word(addr)?.compare_exchange(current, new, Ordering::SeqCst, Ordering::SeqCst))
    }
}

#[cfg(unix)]
mod posix {
    use std::ffi::{c_int, c_void};

    #[cfg(target_vendor = "apple")]
    const MAP_ANON: c_int = 0x1000;
    #[cfg(not(target_vendor = "apple"))]
    const MAP_ANON: c_int = 0x20;
    const MAP_PRIVATE: c_int = 0x2;
    const MAP_FIXED: c_int = 0x10;

    // SAFETY (all declarations): these are the POSIX functions with their C signatures from `<sys/mman.h>`,
    // `<string.h>` and `<unistd.h>`.
    unsafe extern "C" {
        fn mprotect(addr: *mut c_void, len: usize, prot: c_int) -> c_int;
        fn mmap(
            addr: *mut c_void,
            len: usize,
            prot: c_int,
            flags: c_int,
            fd: c_int,
            offset: i64,
        ) -> *mut c_void;
        fn memset(dst: *mut c_void, byte: c_int, len: usize) -> *mut c_void;
        fn memmove(dst: *mut c_void, src: *const c_void, len: usize) -> *mut c_void;
        fn getpagesize() -> c_int;
    }

    pub(super) fn page_size() -> u64 {
        // SAFETY: `getpagesize` has no preconditions.
        u64::try_from(unsafe { getpagesize() }).unwrap_or(0x1000)
    }

    pub(super) fn protect(addr: u64, len: u64, prot: i32) -> bool {
        // SAFETY: the range lies in the host's reservation. The kernel checks it against its model of the
        // reservation before it calls, and `mprotect` only changes the access of whole pages.
        unsafe { mprotect(addr as *mut c_void, len as usize, prot) == 0 }
    }

    /// Replaces the pages with fresh anonymous pages that nothing may access.
    pub(super) fn drop_pages(addr: u64, len: u64) -> bool {
        // SAFETY: as for `protect`. `MAP_FIXED` replaces the pages of the host's reservation, which is the aim.
        let p = unsafe {
            mmap(
                addr as *mut c_void,
                len as usize,
                0,
                MAP_FIXED | MAP_PRIVATE | MAP_ANON,
                -1,
                0,
            )
        };
        p as isize != -1
    }

    pub(super) fn fill_zero(addr: u64, len: u64) -> bool {
        // SAFETY: the kernel calls this for pages that its model marks as accessible.
        unsafe { memset(addr as *mut c_void, 0, len as usize) };
        true
    }

    pub(super) fn copy(dst: u64, src: u64, len: u64) -> bool {
        // SAFETY: as for `fill_zero`, for both ranges.
        unsafe { memmove(dst as *mut c_void, src as *const c_void, len as usize) };
        true
    }

    #[cfg(target_vendor = "apple")]
    pub(super) fn remap(dst: u64, src: u64, len: u64) -> bool {
        const VM_FLAGS_FIXED: c_int = 0;
        const VM_FLAGS_OVERWRITE: c_int = 0x4000;
        const VM_INHERIT_SHARE: c_int = 0;
        // SAFETY (declaration): the C signature of `mach_vm_remap` in `<mach/mach_vm.h>`.
        unsafe extern "C" {
            static mach_task_self_: u32;
            fn mach_vm_remap(
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
        }
        let (mut target, mut cur, mut max) = (dst, 0, 0);
        // SAFETY: both ranges lie in the host's reservation, which the kernel checks before it calls. The call
        // replaces the pages at `dst` with a second mapping of the pages at `src`, which is the aim.
        unsafe {
            mach_vm_remap(
                mach_task_self_,
                &mut target,
                len,
                0,
                VM_FLAGS_FIXED | VM_FLAGS_OVERWRITE,
                mach_task_self_,
                src,
                0,
                &mut cur,
                &mut max,
                VM_INHERIT_SHARE,
            ) == 0
                && target == dst
        }
    }

    #[cfg(not(target_vendor = "apple"))]
    pub(super) fn remap(_dst: u64, _src: u64, _len: u64) -> bool {
        false
    }
}

#[cfg(not(unix))]
mod posix {
    pub(super) fn page_size() -> u64 {
        0x1000
    }
    pub(super) fn protect(_: u64, _: u64, _: i32) -> bool {
        false
    }
    pub(super) fn drop_pages(_: u64, _: u64) -> bool {
        false
    }
    pub(super) fn fill_zero(_: u64, _: u64) -> bool {
        false
    }
    pub(super) fn copy(_: u64, _: u64, _: u64) -> bool {
        false
    }
    pub(super) fn remap(_: u64, _: u64, _: u64) -> bool {
        false
    }
}

/// The host reservation as `HostSpace`. Addresses are host addresses (R18).
///
/// `protect` rounds outward when it grants access and inward when it removes it, so a host page that still has a
/// Horizon page with access stays accessible. Two Horizon pages with different access on one host page get the
/// access of the last call. `release` leaves a host page that it only partly releases readable and writable. Inside the
/// pinned range (`crate::pinned`) `release` zeroes the pages and takes their access instead of replacing them.
impl HostSpace for HostMemory {
    fn page_size(&self) -> u64 {
        posix::page_size().max(PAGE_SIZE)
    }

    fn protect(&self, addr: u64, len: u64, perm: u32) -> bool {
        let ps = self.page_size();
        let (start, end) = if perm & 7 != 0 {
            (addr & !(ps - 1), (addr + len).div_ceil(ps) * ps)
        } else {
            (addr.div_ceil(ps) * ps, (addr + len) & !(ps - 1))
        };
        end <= start || posix::protect(start, end - start, (perm & 7) as i32)
    }

    fn release(&self, addr: u64, len: u64) -> bool {
        let ps = self.page_size();
        let first = addr.div_ceil(ps) * ps;
        let last = (addr + len) & !(ps - 1);
        if last <= first {
            return self.scrub(addr, len);
        }
        if !self.can_remap(addr, len) {
            // the GPU keeps the pages under the range: zero them in place instead of replacing them
            return self.scrub(addr, len) && posix::protect(first, last - first, 0);
        }
        self.drop_view(first, last - first) && self.release_edges(addr, len, first, last)
    }

    fn zero(&self, addr: u64, len: u64) -> bool {
        posix::fill_zero(addr, len)
    }

    fn remap(&self, dst: u64, src: u64, len: u64, perm: u32) -> bool {
        posix::remap(dst, src, len) && self.protect(dst, len, perm)
    }

    fn copy(&self, dst: u64, src: u64, len: u64) -> bool {
        posix::copy(dst, src, len)
    }

    fn can_remap(&self, addr: u64, len: u64) -> bool {
        !crate::pinned::overlaps(addr, len)
    }

    fn drop_view(&self, addr: u64, len: u64) -> bool {
        posix::drop_pages(addr, len)
    }
}

impl HostMemory {
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

/// The system tick frequency in Hz of the console the game was built for: 19.2 MHz on the Switch, 31.25 MHz on the
/// Switch 2.
pub fn tick_hz() -> u64 {
    nxkit_abi::platform::get().tick_hz()
}

/// The system tick from the host's monotonic clock, at [`tick_hz`]. It counts from an arbitrary start and never goes
/// back.
pub fn system_tick() -> u64 {
    nxkit_abi::platform::get().ticks_from_ns(monotonic_ns())
}

#[cfg(target_vendor = "apple")]
fn monotonic_ns() -> u64 {
    #[repr(C)]
    struct TimebaseInfo {
        numer: u32,
        denom: u32,
    }
    // SAFETY (declarations): the C signatures of `<mach/mach_time.h>`.
    unsafe extern "C" {
        fn mach_absolute_time() -> u64;
        fn mach_timebase_info(info: *mut TimebaseInfo) -> i32;
    }
    static SCALE: std::sync::OnceLock<(u64, u64)> = std::sync::OnceLock::new();
    let (numer, denom) = *SCALE.get_or_init(|| {
        let mut info = TimebaseInfo { numer: 1, denom: 1 };
        // SAFETY: `info` is a live local that the call fills.
        let rc = unsafe { mach_timebase_info(&mut info) };
        if rc == 0 && info.denom != 0 {
            (u64::from(info.numer), u64::from(info.denom))
        } else {
            (1, 1)
        }
    });
    // SAFETY: `mach_absolute_time` has no preconditions.
    let ticks = unsafe { mach_absolute_time() };
    (u128::from(ticks) * u128::from(numer) / u128::from(denom)) as u64
}

#[cfg(not(target_vendor = "apple"))]
fn monotonic_ns() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    u64::try_from(
        START
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_nanos(),
    )
    .unwrap_or(u64::MAX)
}
