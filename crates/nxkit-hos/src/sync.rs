//! The lock of the kernel's mutexes: a poisoned mutex is taken as it is, because a panic in one guest thread must not
//! stop the others.

use std::sync::{Mutex, MutexGuard};

/// Takes the lock of `m`, also when another thread panicked while it held it.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
