//! The pointer over the game hides after `NXKIT_CURSOR_HIDE` seconds without movement (3, `0` never), as the dm front
//! end of NX2Apple does it, and comes back with the next movement, click or scroll. It is shown whenever the window is
//! not the key window or the pointer is outside the game, so the title bar and other windows keep theirs.
//! `NXKIT_CURSOR_HIDDEN=1` (a launcher whose own pointer was hidden when it started the game) starts it hidden, so it
//! does not flash up until the first idle check. Holding the mouse for the camera (`mouse.rs`) hides it on its own.
//!
//! Everything here runs on the main thread, so the state is thread-local.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSCursor, NSWindow};

/// How often the idle check looks at the pointer.
const POLL: Duration = Duration::from_millis(250);

struct Idle {
    window: Retained<NSWindow>,
    idle: Duration,
    last: Instant,
    polled: Instant,
    hidden: bool,
    /// A pointer hidden at the start stays so until it has been over the game once: a new window is not key yet.
    armed: bool,
}

thread_local! {
    static IDLE: RefCell<Option<Idle>> = const { RefCell::new(None) };
}

impl Idle {
    fn show(&mut self) {
        if self.hidden {
            self.hidden = false;
            NSCursor::unhide();
        }
    }
}

/// Sets up the idle hide for `window`: after `idle_s` seconds (none at 0), hidden from the start when `hidden`.
pub(super) fn install(window: Retained<NSWindow>, idle_s: f64, hidden: bool) {
    if idle_s <= 0.0 {
        return;
    }
    let start = hidden;
    if start {
        NSCursor::hide();
    }
    let now = Instant::now();
    IDLE.with(|i| {
        *i.borrow_mut() = Some(Idle {
            window,
            idle: Duration::from_secs_f64(idle_s),
            last: now,
            polled: now,
            hidden: start,
            armed: start,
        })
    });
}

/// The mouse moved, clicked or scrolled over the game: the pointer shows.
pub(super) fn activity() {
    IDLE.with(|i| {
        if let Some(idle) = i.borrow_mut().as_mut() {
            idle.last = Instant::now();
            idle.show();
        }
    });
}

/// The display tick: hides the pointer after the idle time over the game, shows it away from the game.
pub(super) fn tick() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    IDLE.with(|i| {
        let mut i = i.borrow_mut();
        let Some(idle) = i.as_mut() else {
            return;
        };
        let now = Instant::now();
        if now.duration_since(idle.polled) < POLL {
            return;
        }
        idle.polled = now;
        let over = NSApplication::sharedApplication(mtm).isActive()
            && idle.window.isKeyWindow()
            && idle.window.contentView().is_some_and(|v| {
                let p =
                    v.convertPoint_fromView(idle.window.mouseLocationOutsideOfEventStream(), None);
                let b = v.bounds();
                p.x >= 0.0 && p.y >= 0.0 && p.x < b.size.width && p.y < b.size.height
            });
        if over {
            idle.armed = false;
        }
        if idle.hidden && !over && !idle.armed {
            idle.show();
        } else if !idle.hidden && over && now.duration_since(idle.last) >= idle.idle {
            idle.hidden = true;
            NSCursor::hide();
        }
    });
}
