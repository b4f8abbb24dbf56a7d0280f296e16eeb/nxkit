//! The game holds the mouse (R63): relative mode for the mouse camera.
//!
//! While the window holds the mouse, the cursor is hidden and stays where it is, and the motion of each mouse event
//! goes to the host as a delta. The window takes the mouse when it becomes the key window or when the player clicks
//! into it, and lets go on Escape, when it loses the key focus (Cmd+Tab) and when it closes. The first click that
//! takes the mouse does not reach the game. Whether the window takes the mouse by itself follows
//! [`AppEvents::mouse_mode`], which the display tick checks, so a controller that connects frees the mouse and one
//! that leaves takes it again.
//!
//! Everything here runs on the main thread, so the state is thread-local.

use std::cell::RefCell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2_app_kit::{NSCursor, NSEvent, NSWindow};
use objc2_core_foundation::{CGPoint, CGRect};

use super::appkit::{AppEvents, MouseEvent, MouseMode};

/// The key code of Escape.
const KEY_ESCAPE: u16 = 53;
/// Motion this soon after the window took the mouse is dropped. The first event after the cursor moved to the
/// centre may carry the jump.
const SETTLE: Duration = Duration::from_millis(50);

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGAssociateMouseAndMouseCursorPosition(connected: u32) -> i32;
    fn CGWarpMouseCursorPosition(point: CGPoint) -> i32;
    fn CGMainDisplayID() -> u32;
    fn CGDisplayBounds(display: u32) -> CGRect;
}

/// The state of the hold.
struct Hold {
    events: Arc<dyn AppEvents>,
    window: Retained<NSWindow>,
    /// When the window took the mouse. `None` while it is free.
    since: Option<Instant>,
    /// The buttons whose release is dropped, because their press took the mouse. Bit `n` is button `n`.
    swallow: u32,
    /// The mode at the last check.
    mode: MouseMode,
}

thread_local! {
    static HOLD: RefCell<Option<Hold>> = const { RefCell::new(None) };
}

fn with(f: impl FnOnce(&mut Hold)) {
    HOLD.with(|h| {
        if let Some(hold) = h.borrow_mut().as_mut() {
            f(hold);
        }
    });
}

/// The centre of the window in the coordinates of Quartz, whose origin is the top left of the main display.
fn centre(window: &NSWindow) -> CGPoint {
    let f = window.frame();
    // SAFETY: both functions take and return plain values.
    let main = unsafe { CGDisplayBounds(CGMainDisplayID()) };
    let x = f.origin.x + f.size.width / 2.0;
    let y = f.origin.y + f.size.height / 2.0;
    CGPoint::new(x, main.size.height - y)
}

impl Hold {
    fn held(&self) -> bool {
        self.since.is_some()
    }

    fn take(&mut self) {
        if self.held() || self.mode == MouseMode::Off {
            return;
        }
        // SAFETY: Quartz calls with plain values. They only change the cursor of this session.
        unsafe {
            CGWarpMouseCursorPosition(centre(&self.window));
            CGAssociateMouseAndMouseCursorPosition(0);
        }
        NSCursor::hide();
        self.since = Some(Instant::now());
    }

    fn release(&mut self) {
        if !self.held() {
            return;
        }
        // SAFETY: a Quartz call with a plain value.
        unsafe { CGAssociateMouseAndMouseCursorPosition(1) };
        NSCursor::unhide();
        self.since = None;
        self.swallow = 0;
        self.events.mouse(MouseEvent::Released);
    }
}

/// Sets up the hold for the window. The window starts with a free mouse.
pub(crate) fn install(events: Arc<dyn AppEvents>, window: Retained<NSWindow>) {
    let mode = events.mouse_mode();
    HOLD.with(|h| *h.borrow_mut() = Some(Hold { events, window, since: None, swallow: 0, mode }));
}

/// The window became the key window or lost it.
pub(crate) fn focus(focused: bool) {
    with(|h| {
        h.mode = h.events.mouse_mode();
        if !focused {
            h.release();
        } else if h.mode == MouseMode::Auto {
            h.take();
        }
    });
}

/// Lets go of the mouse, for example when the window closes.
pub(crate) fn release() {
    with(Hold::release);
}

/// The display tick: follows a change of the mode. A controller that connects frees the mouse, one that leaves
/// takes it while the window is key.
pub(crate) fn tick() {
    with(|h| {
        let mode = h.events.mouse_mode();
        if mode == h.mode {
            return;
        }
        h.mode = mode;
        match mode {
            MouseMode::Auto if h.window.isKeyWindow() => h.take(),
            MouseMode::Auto => {}
            MouseMode::Click | MouseMode::Off => h.release(),
        }
    });
}

/// A mouse motion event.
pub(crate) fn moved(event: &NSEvent) {
    with(|h| {
        if h.since.is_some_and(|t| t.elapsed() >= SETTLE) {
            h.events.mouse(MouseEvent::Moved { dx: event.deltaX(), dy: event.deltaY() });
        }
    });
}

/// A mouse button went down or up. A press while the mouse is free takes it and does not reach the game.
pub(crate) fn button(event: &NSEvent, down: bool) {
    let number = u8::try_from(event.buttonNumber()).unwrap_or(u8::MAX);
    let bit = 1u32.checked_shl(u32::from(number)).unwrap_or(0);
    with(|h| {
        if down && !h.held() {
            h.take();
            if h.held() {
                h.swallow |= bit;
            }
            return;
        }
        if !down && h.swallow & bit != 0 {
            h.swallow &= !bit;
            return;
        }
        if h.held() {
            h.events.mouse(MouseEvent::Button { number, down });
        }
    });
}

/// A scroll event. Only a wheel notch or the start of a trackpad scroll matters, the service makes it a short
/// press.
pub(crate) fn wheel(event: &NSEvent) {
    with(|h| {
        let dy = event.scrollingDeltaY();
        if h.held() && dy != 0.0 {
            h.events.mouse(MouseEvent::Wheel { dy });
        }
    });
}

/// A key went down. Escape lets go of the mouse.
pub(crate) fn key_down(event: &NSEvent) {
    if event.keyCode() == KEY_ESCAPE {
        release();
    }
}
