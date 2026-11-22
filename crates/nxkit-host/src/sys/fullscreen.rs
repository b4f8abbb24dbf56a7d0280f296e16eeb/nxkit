//! Full screen at start (`--fullscreen`, `NXKIT_FULLSCREEN`).
//!
//! A game that a launcher opens from its own full-screen Space starts while macOS leaves that Space. A
//! `toggleFullScreen:` asked for then is refused, or the window goes full screen on the Space that is going away. So
//! the request waits until the application is active and the window is on the active Space, whichever comes last, and
//! a refused request is asked once more. A transition that the player starts while the start waits (the green button,
//! Control-Command-F) ends the start request: it never toggles the window back.

use std::cell::Cell;

use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSWindow, NSWindowStyleMask, NSWorkspace,
    NSWorkspaceActiveSpaceDidChangeNotification,
};

/// Toggles `window` into full screen when the start request may now.
pub(crate) fn try_enter(mtm: MainThreadMarker, window: &NSWindow, state: &Cell<Startup>) {
    let mut s = state.get();
    let active = NSApplication::sharedApplication(mtm).isActive();
    let full = window.styleMask().contains(NSWindowStyleMask::FullScreen);
    let go = s.poll(active, window.isOnActiveSpace(), full);
    state.set(s);
    if go {
        window.toggleFullScreen(None);
    }
}

/// The window did not enter full screen: ask once more on the next turn of the run loop.
pub(crate) fn refused(
    mtm: MainThreadMarker,
    delegate: &AnyObject,
    window: &NSWindow,
    state: &Cell<Startup>,
) {
    let mut s = state.get();
    let refusal = s.failed();
    state.set(s);
    let again = match refusal {
        Refusal::NotOurs => return,
        Refusal::AskAgain => true,
        Refusal::GiveUp => false,
    };
    let active = NSApplication::sharedApplication(mtm).isActive();
    eprintln!(
        "nxkit-host: full screen was refused (application active {active}, window on the active Space {}){}",
        window.isOnActiveSpace(),
        if again { ", asking once more" } else { "" }
    );
    if again {
        later(delegate);
    }
}

/// Calls `nxkitEnterFullScreen` of `delegate` on the next turn of the run loop, after the current transition or start.
pub(crate) fn later(delegate: &AnyObject) {
    let none: Option<&AnyObject> = None;
    // SAFETY: `performSelector:withObject:afterDelay:` of `NSObject` on the main thread. The delegate implements the
    // selector with no argument (the nil object is ignored) and lives for the whole run of the application.
    unsafe {
        let _: () = msg_send![delegate, performSelector: sel!(nxkitEnterFullScreen), withObject: none, afterDelay: 0.0f64];
    }
}

/// Sends the Space changes of the workspace to `nxkitActiveSpaceDidChange:` of `delegate`, or stops (`on` false).
pub(crate) fn observe_spaces(delegate: &AnyObject, on: bool) {
    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    // SAFETY: the delegate implements `nxkitActiveSpaceDidChange:` with one `NSNotification` argument, lives for the
    // whole run of the application and is removed before `run` returns.
    unsafe {
        if on {
            center.addObserver_selector_name_object(
                delegate,
                sel!(nxkitActiveSpaceDidChange:),
                Some(NSWorkspaceActiveSpaceDidChangeNotification),
                None,
            );
        } else {
            center.removeObserver(delegate);
        }
    }
}

/// What a refused full screen means for the start request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The request was the player's, not the start's.
    NotOurs,
    /// The start asks once more.
    AskAgain,
    /// The start was refused twice and the window stays as it is.
    GiveUp,
}

/// The state of the start request. One per window, on the main thread.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Startup {
    /// The window should still go full screen.
    want: bool,
    /// Our own `toggleFullScreen:` is under way.
    entering: bool,
    /// A refused request was asked once more.
    retried: bool,
}

impl Startup {
    /// A start request when `on`, else one that never toggles.
    pub(crate) fn new(on: bool) -> Self {
        Startup {
            want: on,
            ..Startup::default()
        }
    }

    /// Something changed (the start, the activation, the Space). True when the window must toggle now. The request
    /// then counts as under way.
    pub(crate) fn poll(&mut self, active: bool, on_active_space: bool, full_screen: bool) -> bool {
        if !self.want || full_screen || !active || !on_active_space {
            return false;
        }
        self.want = false;
        self.entering = true;
        true
    }

    /// The window will enter or leave full screen. A transition that is not ours is the player's, and it ends the
    /// start request.
    pub(crate) fn will_transition(&mut self) {
        self.want = false;
    }

    /// The window is in full screen: nothing is left to do.
    pub(crate) fn entered(&mut self) {
        *self = Startup::default();
    }

    /// The window did not enter full screen. After [`Refusal::AskAgain`] the next `poll` toggles again.
    pub(crate) fn failed(&mut self) -> Refusal {
        if !self.entering {
            return Refusal::NotOurs;
        }
        self.entering = false;
        if self.retried {
            return Refusal::GiveUp;
        }
        self.retried = true;
        self.want = true;
        Refusal::AskAgain
    }
}

#[cfg(test)]
mod tests {
    use super::{Refusal, Startup};

    #[test]
    fn waits_for_the_activation_and_the_space() {
        let mut s = Startup::new(true);
        assert!(!s.poll(false, true, false));
        assert!(!s.poll(true, false, false));
        assert!(s.poll(true, true, false));
        assert!(!s.poll(true, true, false), "one request, not one per poll");
    }

    #[test]
    fn off_never_toggles_and_a_full_screen_window_stays() {
        assert!(!Startup::new(false).poll(true, true, false));
        assert!(!Startup::new(true).poll(true, true, true));
    }

    #[test]
    fn a_refusal_is_asked_once_more() {
        let mut s = Startup::new(true);
        assert!(s.poll(true, true, false));
        s.will_transition();
        assert_eq!(s.failed(), Refusal::AskAgain);
        assert!(s.poll(true, true, false));
        assert_eq!(s.failed(), Refusal::GiveUp);
        assert!(!s.poll(true, true, false));
    }

    #[test]
    fn the_player_takes_over_while_the_start_waits() {
        let mut s = Startup::new(true);
        assert!(!s.poll(false, false, false));
        s.will_transition();
        assert_eq!(s.failed(), Refusal::NotOurs);
        assert!(!s.poll(true, true, false));
    }

    #[test]
    fn entered_ends_the_request() {
        let mut s = Startup::new(true);
        assert!(s.poll(true, true, false));
        s.entered();
        assert_eq!(s.failed(), Refusal::NotOurs);
        assert!(!s.poll(true, true, false));
    }
}
