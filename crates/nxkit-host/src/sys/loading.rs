//! The loading screen of the start: a small window with a progress bar and the line "Preparing pipelines: x of y".
//!
//! The encoder (`nxkit-gpu`) makes the pipelines that an earlier build recorded before the first frame, and a rebuilt
//! app can wait for a while for the ones its archive lacks (`pipelines/prewarm.rs`). The window of the game does not
//! exist yet at that point: the encoder is made first and the window opens when the application loop starts. So this
//! is a window of its own, made of stock AppKit views (no Metal, no game involvement), that exists only while the
//! start waits and is closed when it ends. The game's window then opens as usual.
//!
//! The encoder calls [`LoadingScreen`] on the thread that waits, the main thread, about ten times a second and not
//! before a second has passed. Each call sets the bar and the text and lets the run loop work for a few milliseconds,
//! because the application loop is not running yet and the window would not be drawn otherwise. A call from another
//! thread does nothing.

use std::cell::RefCell;
use std::ffi::c_void;

use nxkit_gpu::PrewarmScreen;
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSProgressIndicator,
    NSProgressIndicatorStyle, NSTextField, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::NSString;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopDefaultMode: *const c_void;
    fn CFRunLoopRunInMode(
        mode: *const c_void,
        seconds: f64,
        return_after_source_handled: u8,
    ) -> i32;
}

/// The window and its views, on the main thread.
struct Shown {
    window: Retained<NSWindow>,
    label: Retained<NSTextField>,
    bar: Retained<NSProgressIndicator>,
}

thread_local! {
    static SHOWN: RefCell<Option<Shown>> = const { RefCell::new(None) };
}

/// The text under the bar.
pub fn text(done: usize, total: usize) -> String {
    format!("Preparing pipelines: {done} of {total}")
}

/// Lets the run loop draw and deliver events for a moment.
fn spin() {
    // SAFETY: a plain call on the main thread with the default mode of the run loop.
    unsafe {
        CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.005, 0);
    }
}

fn make(mtm: MainThreadMarker, total: usize) -> Shown {
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let rect = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(420.0, 96.0));
    // SAFETY: a fresh `NSWindow` with valid arguments, on the main thread.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect,
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: the window is kept in `Retained` and closed by `finish`. AppKit must not release it again on close.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str("nxkit"));
    let content: Retained<NSView> = NSView::initWithFrame(NSView::alloc(mtm), rect);
    let label = NSTextField::labelWithString(&NSString::from_str(&text(0, total)), mtm);
    label.setFrame(CGRect::new(
        CGPoint::new(20.0, 52.0),
        CGSize::new(380.0, 22.0),
    ));
    let bar = NSProgressIndicator::initWithFrame(
        NSProgressIndicator::alloc(mtm),
        CGRect::new(CGPoint::new(20.0, 24.0), CGSize::new(380.0, 16.0)),
    );
    bar.setStyle(NSProgressIndicatorStyle::Bar);
    bar.setIndeterminate(false);
    bar.setMinValue(0.0);
    bar.setMaxValue(total.max(1) as f64);
    content.addSubview(&label);
    content.addSubview(&bar);
    window.setContentView(Some(&content));
    window.center();
    window.makeKeyAndOrderFront(None);
    app.activate();
    Shown { window, label, bar }
}

/// The loading screen that the host hands to the encoder.
pub struct LoadingScreen;

impl PrewarmScreen for LoadingScreen {
    fn update(&self, done: usize, total: usize) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        SHOWN.with(|s| {
            let mut s = s.borrow_mut();
            let shown = s.get_or_insert_with(|| make(mtm, total));
            shown.bar.setDoubleValue(done as f64);
            shown
                .label
                .setStringValue(&NSString::from_str(&text(done, total)));
            shown.window.displayIfNeeded();
        });
        spin();
    }

    fn finish(&self) {
        if MainThreadMarker::new().is_none() {
            return;
        }
        let shown = SHOWN.with(|s| s.borrow_mut().take());
        if let Some(s) = shown {
            s.window.close();
            spin();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_says_how_far_it_is() {
        assert_eq!(text(12, 3400), "Preparing pipelines: 12 of 3400");
    }
}
