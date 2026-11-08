//! A counting semaphore that the audio callback can signal without taking a lock.

#![allow(unsafe_code)]

#[cfg(target_vendor = "apple")]
mod imp {
    use std::ffi::c_void;

    unsafe extern "C" {
        fn dispatch_semaphore_create(value: isize) -> *mut c_void;
        fn dispatch_semaphore_signal(dsema: *mut c_void) -> isize;
        fn dispatch_semaphore_wait(dsema: *mut c_void, timeout: u64) -> isize;
        fn dispatch_time(when: u64, delta: i64) -> u64;
    }

    /// The semaphore. It lives until the process ends.
    pub struct Sem {
        raw: *mut c_void,
    }

    // SAFETY: a dispatch semaphore may be used from any thread.
    unsafe impl Send for Sem {}
    // SAFETY: a dispatch semaphore may be used from any thread.
    unsafe impl Sync for Sem {}

    impl Sem {
        /// A semaphore with the count 0.
        pub fn new() -> Sem {
            // SAFETY: `dispatch_semaphore_create` has no preconditions for a count of 0.
            let raw = unsafe { dispatch_semaphore_create(0) };
            Sem { raw }
        }

        /// Adds one to the count and wakes a waiter. Safe in the audio callback.
        pub fn signal(&self) {
            if !self.raw.is_null() {
                // SAFETY: `raw` is a live semaphore.
                unsafe { dispatch_semaphore_signal(self.raw) };
            }
        }

        /// Waits for a signal for up to `ms` milliseconds.
        pub fn wait_ms(&self, ms: u64) {
            if self.raw.is_null() {
                std::thread::sleep(std::time::Duration::from_millis(ms));
                return;
            }
            // SAFETY: `dispatch_time` only computes a number. `raw` is a live semaphore.
            unsafe {
                let deadline = dispatch_time(
                    0,
                    i64::try_from(ms.saturating_mul(1_000_000)).unwrap_or(i64::MAX),
                );
                dispatch_semaphore_wait(self.raw, deadline);
            }
        }
    }
}

#[cfg(not(target_vendor = "apple"))]
mod imp {
    use std::sync::{Condvar, Mutex};
    use std::time::Duration;

    /// The semaphore. Other platforms have no audio output here, so the plain version is enough.
    pub struct Sem {
        count: Mutex<u32>,
        cv: Condvar,
    }

    impl Sem {
        /// A semaphore with the count 0.
        pub fn new() -> Sem {
            Sem {
                count: Mutex::new(0),
                cv: Condvar::new(),
            }
        }

        /// Adds one to the count and wakes a waiter.
        pub fn signal(&self) {
            if let Ok(mut c) = self.count.lock() {
                *c = c.saturating_add(1);
                self.cv.notify_one();
            }
        }

        /// Waits for a signal for up to `ms` milliseconds.
        pub fn wait_ms(&self, ms: u64) {
            if let Ok(c) = self.count.lock() {
                if let Ok((mut c, _)) =
                    self.cv
                        .wait_timeout_while(c, Duration::from_millis(ms), |c| *c == 0)
                {
                    *c = c.saturating_sub(1);
                }
            }
        }
    }
}

pub use imp::Sem;

impl Default for Sem {
    fn default() -> Self {
        Sem::new()
    }
}
