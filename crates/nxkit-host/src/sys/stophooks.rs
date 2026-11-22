//! The frame hooks of the window: they pass every hook to the host and end the application after a stop request
//! (`appkit.rs`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use nxkit_metal::{DisplayTick, FrameHooks};
use objc2::MainThreadMarker;
use objc2_app_kit::NSApplication;

use super::{homeui, mouse, present};

/// Passes every hook to the host and ends the application on the next tick after a stop request. The tick is the
/// only code that runs on the main thread by itself, so the stop request waits for it.
pub(super) struct StopHooks {
    pub(super) inner: Arc<dyn FrameHooks>,
    pub(super) stop: Arc<AtomicBool>,
    /// The GPU reports the first picture that shows something, so a finished frame does not count.
    pub(super) probed: bool,
}

impl FrameHooks for StopHooks {
    fn frame_begin(&self, frame: u64) {
        self.inner.frame_begin(frame);
    }

    fn before_present(&self, frame: u64) {
        self.inner.before_present(frame);
    }

    fn frame_complete(&self, stats: &nxkit_metal::CommandBufferStats) {
        if !self.probed {
            present::picture_done();
        }
        self.inner.frame_complete(stats);
    }

    fn display_tick(&self, tick: DisplayTick) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        if self.stop.load(Ordering::Acquire) {
            // An overlay fades out first and ends a fade later (`present.rs`).
            if present::may_end() {
                NSApplication::sharedApplication(mtm).terminate(None);
            }
            return;
        }
        present::tick(mtm);
        mouse::tick();
        super::pointer::tick();
        homeui::tick();
        super::textinputui::tick();
        self.inner.display_tick(tick);
    }
}
