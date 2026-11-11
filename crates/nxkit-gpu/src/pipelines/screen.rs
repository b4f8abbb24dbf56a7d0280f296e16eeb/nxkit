//! The loading screen of the start, as a hook of the host.
//!
//! The start (`prewarm.rs`) can take a while: the first start of an app build that was played before compiles the
//! pipelines that the shipped archive lacks, and it waits for all of them. The GPU crate has no window, and the
//! window of the game does not exist yet at that point (the encoder is made before it). So the host sets a screen,
//! and the start tells it how far it is. The calls come on the thread that waits, which is the one that made the
//! encoder (the main thread of the host). The first `update` comes after a second of waiting, so a start that is
//! quick shows nothing; `finish` comes once when the wait ends, if `update` was called.

use std::sync::{Arc, Mutex, PoisonError};

/// What the host shows while the start makes the pipelines of the list.
pub trait PrewarmScreen: Send + Sync {
    /// `done` of `total` pipelines are made. Called about ten times a second while the start waits.
    fn update(&self, done: usize, total: usize);
    /// The start no longer waits: take the screen away.
    fn finish(&self);
}

static SCREEN: Mutex<Option<Arc<dyn PrewarmScreen>>> = Mutex::new(None);

/// Sets (or, with `None`, removes) the screen of the start. The host calls it before it makes the encoder.
pub fn set_prewarm_screen(screen: Option<Arc<dyn PrewarmScreen>>) {
    *SCREEN.lock().unwrap_or_else(PoisonError::into_inner) = screen;
}

pub(super) fn current() -> Option<Arc<dyn PrewarmScreen>> {
    SCREEN
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Count(AtomicUsize);
    impl PrewarmScreen for Count {
        fn update(&self, done: usize, _total: usize) {
            self.0.fetch_add(done, Ordering::Relaxed);
        }
        fn finish(&self) {
            self.0.fetch_add(1000, Ordering::Relaxed);
        }
    }

    #[test]
    fn the_host_sets_and_removes_the_screen() {
        let c = Arc::new(Count(AtomicUsize::new(0)));
        set_prewarm_screen(Some(c.clone()));
        let s = current().expect("a screen");
        s.update(5, 10);
        s.finish();
        assert_eq!(c.0.load(Ordering::Relaxed), 1005);
        set_prewarm_screen(None);
        assert!(current().is_none());
    }
}
