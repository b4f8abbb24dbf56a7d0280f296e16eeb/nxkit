//! How the game's window appears and goes.
//!
//! With `NXKIT_OVERLAY=1` (a launcher in full screen, NX2Apple's console mode) the game does not open a Space of its
//! own. Its window is borderless, covers the screen and joins every Space, the launcher's full-screen Space too
//! (`fullScreenAuxiliary`), so it opens over the launcher with no Space change. It stays transparent until the game's
//! picture shows something (a game presents black pictures while it boots, and `nxkit_gpu::when_shown` looks at them)
//! and then fades in over the launcher's own starting screen. The launcher hears it as `org.nxkit.shown.<pid>`. Only
//! then does the application ask for the front: active with no window on the launcher's Space, it would take the player
//! to the desktop. When the game ends, the window fades out before the application quits, and the launcher is
//! right under it. The loading screen of the start is the launcher's, which gets the progress (`loading.rs`).
//!
//! Without it the window is the titled window of `appkit.rs`. Both get a black background, so nothing white shows before
//! the first picture.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAnimationContext, NSApplication, NSApplicationPresentationOptions, NSBackingStoreType,
    NSColor, NSResponder, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::CGRect;
use objc2_foundation::NSObject;

use super::notify;

/// The fade of the window, in and out (`motion.standard` of NX2Apple's tokens, a little longer for a whole screen).
const FADE: Duration = Duration::from_millis(300);
/// A game that has shown nothing after this still shows, so the player is not left with the launcher.
const SHOW_ANYWAY: Duration = Duration::from_secs(45);
/// `NSFloatingWindowLevel`: above the launcher's full-screen window on its Space.
const FLOATING: isize = 3;

// Process-wide (W26): the first picture is done on the thread of a Metal completion handler, which has no context. The
// display tick on the main thread reads it.
static FIRST_PICTURE: AtomicBool = AtomicBool::new(false);

define_class!(
    // SAFETY:
    // - `NSWindow` has no subclassing requirements beyond calling its designated initialiser, which `overlay_window`
    //   does.
    // - `MainThreadOnly` is correct, because AppKit uses windows on the main thread only.
    // - `GameWindow` does not implement `Drop`.
    /// A borderless window that can be the key window: the game reads the keyboard and the Home button only then.
    #[unsafe(super(NSWindow, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NxkitGameWindow"]
    struct GameWindow;

    impl GameWindow {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key(&self) -> bool {
            true
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main(&self) -> bool {
            true
        }
    }
);

/// The window of the overlay: borderless over `frame` (the screen), on every Space, above the launcher, transparent
/// until the first picture.
pub(super) fn overlay_window(mtm: MainThreadMarker, frame: CGRect) -> Retained<NSWindow> {
    // SAFETY: the designated initialiser of `NSWindow` on a fresh instance, with valid arguments.
    let window: Retained<GameWindow> = unsafe {
        msg_send![
            GameWindow::alloc(mtm),
            initWithContentRect: frame,
            styleMask: NSWindowStyleMask::Borderless,
            backing: NSBackingStoreType::Buffered,
            defer: false
        ]
    };
    let window: Retained<NSWindow> = Retained::into_super(window);
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    window.setLevel(FLOATING);
    window.setHasShadow(false);
    window.setAlphaValue(0.0);
    window.setFrame_display(frame, false);
    window
}

/// Puts the overlay on the screen before it shows: transparent, clicks go to the launcher under it, and the application
/// stays inactive. The display link of the window's layer ticks only while the window is on a screen, and the fade-in
/// waits for a tick: a window kept off the screen until then never showed.
fn order_in(window: &NSWindow) {
    window.setIgnoresMouseEvents(true);
    window.orderFrontRegardless();
}

/// The background of either window: black, so nothing white shows before the first picture. The views over it draw
/// nothing of their own.
pub(super) fn black(window: &NSWindow) {
    window.setBackgroundColor(Some(&NSColor::blackColor()));
}

struct Present {
    window: Retained<NSWindow>,
    overlay: bool,
    since: Instant,
    shown: bool,
    closing: Option<Instant>,
}

thread_local! {
    // Main thread only: the display tick and the stop run there.
    static PRESENT: RefCell<Option<Present>> = const { RefCell::new(None) };
}

/// Sets up the appearing of `window`, an overlay when `overlay`. An overlay goes on the screen now (`order_in`).
pub(super) fn install(window: Retained<NSWindow>, overlay: bool) {
    if overlay {
        order_in(&window);
    }
    PRESENT.with(|p| {
        *p.borrow_mut() = Some(Present {
            window,
            overlay,
            since: Instant::now(),
            shown: !overlay,
            closing: None,
        })
    });
}

/// The game's picture shows something (`nxkit_gpu::when_shown`), or a frame is done where that cannot be told (a
/// Vulkan game). Any thread.
pub(crate) fn picture_done() {
    FIRST_PICTURE.store(true, Ordering::Release);
}

/// The window is an overlay over a launcher.
pub(super) fn is_overlay() -> bool {
    PRESENT.with(|p| p.borrow().as_ref().is_some_and(|p| p.overlay))
}

fn fade(window: &NSWindow, alpha: f64) {
    NSAnimationContext::beginGrouping();
    NSAnimationContext::currentContext().setDuration(FADE.as_secs_f64());
    // SAFETY: `animator` of `NSWindow` (`NSAnimatablePropertyContainer`) returns a proxy of the window.
    let proxy: Retained<NSWindow> = unsafe { msg_send![window, animator] };
    proxy.setAlphaValue(alpha);
    NSAnimationContext::endGrouping();
}

/// The display tick: the overlay fades in once the first picture is done.
pub(super) fn tick(mtm: MainThreadMarker) {
    PRESENT.with(|p| {
        let mut p = p.borrow_mut();
        let Some(p) = p.as_mut() else { return };
        if p.shown || p.closing.is_some() {
            return;
        }
        let first = FIRST_PICTURE.load(Ordering::Acquire);
        if !first && p.since.elapsed() < SHOW_ANYWAY {
            return;
        }
        p.shown = true;
        let app = NSApplication::sharedApplication(mtm);
        app.setPresentationOptions(
            NSApplicationPresentationOptions::HideDock
                | NSApplicationPresentationOptions::HideMenuBar,
        );
        p.window.setIgnoresMouseEvents(false);
        p.window.makeKeyAndOrderFront(None);
        app.activate();
        fade(&p.window, 1.0);
        eprintln!(
            "nxkit-host: the game shows over the launcher{}",
            if first { "" } else { " (no picture yet)" }
        );
        notify::post("shown");
    });
}

/// The game is to end. True when the application may quit now. An overlay fades out first and says so a fade later.
pub(super) fn may_end() -> bool {
    PRESENT.with(|p| {
        let mut p = p.borrow_mut();
        let Some(p) = p.as_mut() else { return true };
        if !p.overlay || !p.shown {
            return true;
        }
        match p.closing {
            None => {
                p.closing = Some(Instant::now());
                fade(&p.window, 0.0);
                false
            }
            Some(t) => t.elapsed() >= FADE,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fade_is_short() {
        assert!(FADE < Duration::from_secs(1) && SHOW_ANYWAY > Duration::from_secs(5));
    }
}
