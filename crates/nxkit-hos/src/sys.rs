//! The only module with `unsafe`: address waits, thread QoS and raw guest memory.
//!
//! On Apple platforms the waits are `os_sync_wait_on_address` and `os_sync_wake_by_address_*` (macOS 14.4, iOS 17.4).
//! Other platforms get a safe fallback of sharded mutexes and condition variables, so the crate builds and tests there.

#![allow(unsafe_code)]

use crate::memory::GuestMemory;
use crate::role::Qos;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

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
