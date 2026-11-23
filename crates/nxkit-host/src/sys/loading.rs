//! The loading screen of the start: a small card in the look of NX2Apple's console mode (`look.rs`) with the game's
//! name, the line "Preparing pipelines: x of y" and a cream bar. Over a launcher (`NXKIT_OVERLAY=1`) no window shows:
//! the launcher gets the progress as `org.nxkit.loading.<pid>` and shows it on its own starting screen.
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
use std::time::{Duration, Instant};

use nxkit_gpu::PrewarmScreen;
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSBox, NSColor, NSTextField,
    NSView, NSWindow, NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::NSString;

use super::font::{self, Weight};
use super::look::{label, rect, rgb, surface, tokens};
use super::notify;

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
    fill: Retained<NSBox>,
    /// The frame of the bar's track, which the fill grows in.
    track: CGRect,
}

/// The card: its size, the padding, the bar's height.
const SIZE: (f64, f64) = (460.0, 136.0);
const PAD: f64 = 24.0;
const BAR: f64 = 6.0;
/// The launcher gets the progress at most this often.
const POST_EVERY: Duration = Duration::from_millis(250);

thread_local! {
    // Main thread only: the encoder calls the screen there.
    static SHOWN: RefCell<Option<Shown>> = const { RefCell::new(None) };
    static TITLE: RefCell<String> = const { RefCell::new(String::new()) };
    static POSTED: RefCell<Option<Instant>> = const { RefCell::new(None) };
}

/// The title of the loading screen: the game's name. On the main thread, before the start waits.
pub fn set_title(name: &str) {
    TITLE.with(|t| *t.borrow_mut() = name.to_owned());
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
    let (w, h) = SIZE;
    let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w, h));
    // SAFETY: a fresh `NSWindow` with valid arguments, on the main thread.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: the window is kept in `Retained` and closed by `finish`. AppKit must not release it again on close.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setOpaque(false);
    window.setBackgroundColor(Some(&NSColor::clearColor()));
    window.setHasShadow(true);
    let name = TITLE.with(|t| t.borrow().clone());
    let (content, line, fill, track) = card(mtm, &name, 0, total);
    window.setContentView(Some(&content));
    window.center();
    window.makeKeyAndOrderFront(None);
    // The screen shows before the run loop runs, and an activation asked for then can get lost (cooperative
    // activation, see `appkit.rs`): the window stayed behind the terminal or the launcher. It goes in front without the
    // activation.
    window.orderFrontRegardless();
    app.activate();
    Shown {
        window,
        label: line,
        fill,
        track,
    }
}

/// The card: the game's name (`nxkit` without one), the line and the bar at `done` of `total`.
fn card(
    mtm: MainThreadMarker,
    name: &str,
    done: usize,
    total: usize,
) -> (
    Retained<NSView>,
    Retained<NSTextField>,
    Retained<NSBox>,
    CGRect,
) {
    let (w, h) = SIZE;
    let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w, h));
    let content: Retained<NSView> = NSView::initWithFrame(NSView::alloc(mtm), frame);
    let hairline = rgb(tokens::WHITE, tokens::HAIRLINE);
    content.addSubview(&surface(
        mtm,
        frame,
        &rgb(tokens::SURFACE, 1.0),
        20.0,
        (&hairline, 1.0),
    ));
    let inner = w - 2.0 * PAD;
    let name = if name.is_empty() { "nxkit" } else { name };
    let (title, ts) = label(
        mtm,
        name,
        &font::manrope(20.0, Weight::Bold),
        &rgb(tokens::WHITE, tokens::TEXT_PRIMARY),
        inner,
        false,
    );
    let (line, ls) = label(
        mtm,
        &text(done, total),
        &font::manrope(15.0, Weight::Regular),
        &rgb(tokens::WHITE, tokens::TEXT_SECONDARY),
        inner,
        false,
    );
    // AppKit's y goes up: the title at the top, the line under it, the bar at the bottom.
    title.setFrame(rect(PAD, h - PAD - ts.height, inner, ts.height));
    line.setFrame(rect(
        PAD,
        h - PAD - ts.height - 6.0 - ls.height,
        inner,
        ls.height,
    ));
    let track = rect(PAD, PAD, inner, BAR);
    let quiet = rgb(tokens::WHITE, tokens::QUIET_FILL);
    let none = NSColor::clearColor();
    content.addSubview(&title);
    content.addSubview(&line);
    content.addSubview(&surface(mtm, track, &quiet, BAR / 2.0, (&none, 0.0)));
    let fill = surface(
        mtm,
        rect(PAD, PAD, (inner * share(done, total)).max(BAR), BAR),
        &rgb(tokens::ACCENT, 1.0),
        BAR / 2.0,
        (&none, 0.0),
    );
    content.addSubview(&fill);
    (content, line, fill, track)
}

/// The card alone, for `home-preview`.
pub(super) fn preview(
    mtm: MainThreadMarker,
    name: &str,
    done: usize,
    total: usize,
) -> Retained<NSView> {
    card(mtm, name, done, total).0
}

/// The share of the bar that is done.
fn share(done: usize, total: usize) -> f64 {
    if total == 0 {
        1.0
    } else {
        (done as f64 / total as f64).clamp(0.0, 1.0)
    }
}

/// Tells the launcher how far the start is, at most every [`POST_EVERY`] and always at the end.
fn post(done: usize, total: usize) {
    POSTED.with(|p| {
        let mut p = p.borrow_mut();
        if done < total && p.is_some_and(|t| t.elapsed() < POST_EVERY) {
            return;
        }
        *p = Some(Instant::now());
        notify::post_progress(done, total);
    });
}

/// The loading screen that the host hands to the encoder.
pub struct LoadingScreen;

impl PrewarmScreen for LoadingScreen {
    fn update(&self, done: usize, total: usize) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        // Over a launcher the start is the launcher's own screen: it gets the progress, and no window shows.
        if crate::hostenv::host().overlay {
            post(done, total);
            return;
        }
        SHOWN.with(|s| {
            let mut s = s.borrow_mut();
            let shown = s.get_or_insert_with(|| make(mtm, total));
            let t = shown.track;
            let width = (t.size.width * share(done, total)).max(BAR);
            shown
                .fill
                .setFrame(rect(t.origin.x, t.origin.y, width, t.size.height));
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
    fn the_bar_follows_the_share() {
        assert_eq!(share(0, 0), 1.0);
        assert_eq!(share(5, 10), 0.5);
        assert_eq!(share(12, 10), 1.0);
    }

    #[test]
    fn the_line_says_how_far_it_is() {
        assert_eq!(text(12, 3400), "Preparing pipelines: 12 of 3400");
    }
}
