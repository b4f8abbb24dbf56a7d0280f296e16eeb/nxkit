//! Signal completion of the workers even if archive cleanup unwinds.

use std::path::Path;
use std::sync::{Condvar, Mutex, PoisonError};

struct WakeOnDrop<'a>(&'a (Mutex<bool>, Condvar));

impl Drop for WakeOnDrop<'_> {
    fn drop(&mut self) {
        let (done, wake) = self.0;
        *done.lock().unwrap_or_else(PoisonError::into_inner) = true;
        wake.notify_all();
    }
}

pub(super) fn complete(
    over: &(Mutex<bool>, Condvar),
    marker: Option<&Path>,
    flush: impl FnOnce() -> bool,
) {
    let _wake = WakeOnDrop(over);
    if flush()
        && let Some(path) = marker
        && let Err(e) = std::fs::write(path, b"")
    {
        eprintln!("nxkit-gpu: {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_or_panicking_cleanup_wakes_the_waiter_without_marking_the_cache() {
        let marker =
            std::env::temp_dir().join(format!("nxkit-failed-prewarm-{}", std::process::id()));
        for unwind in [false, true] {
            let over = (Mutex::new(false), Condvar::new());
            let result = std::panic::catch_unwind(|| {
                complete(&over, Some(&marker), || {
                    if unwind {
                        std::panic::resume_unwind(Box::new("test cleanup"));
                    }
                    false
                })
            });
            assert_eq!(result.is_err(), unwind);
            assert!(*over.0.lock().unwrap());
            assert!(!marker.exists());
        }
    }
}
