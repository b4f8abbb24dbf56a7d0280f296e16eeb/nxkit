//! The Home button on the window (`home.rs`, the views in `homeviews.rs`).
//!
//! The display tick reads the buttons of the controllers and steps the Home menu while the application is active and
//! the window has the key focus. A screen that opens takes the controllers from the game (`AppEvents::home_input`)
//! and tells the game it lost the focus, as the console's HOME menu does. Quit ends the game through the same stop as
//! a window close.
//!
//! Back to the launcher pauses the game in the background: the game is told it is in the background, the window
//! leaves full screen so its Space goes away, the launcher (`NXKIT_LAUNCHER_PID`, else `NXKIT_LAUNCHER_BUNDLE_ID`) gets
//! the activation and the application hides. It comes back when it is active again (the launcher activates it, the
//! Dock, Command-Tab), when it is shown again, or when the launcher posts `org.nxkit.resume.<pid>`.
//! `org.nxkit.quit.<pid>` quits it. The state goes out as the distributed notifications `org.nxkit.paused.<pid>` and
//! `org.nxkit.resumed.<pid>`.

use std::cell::RefCell;
use std::sync::Arc;
use std::time::Instant;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationOptions, NSRunningApplication, NSView, NSWindow,
    NSWindowDidExitFullScreenNotification, NSWindowStyleMask,
};
use objc2_foundation::{
    NSDistributedNotificationCenter, NSNotification, NSNotificationCenter, NSObject,
    NSObjectProtocol, NSString,
};

use super::appkit::AppEvents;
use super::homeviews::{self, Card, HoldRing};
use super::notify;
use crate::home::{ANSWER_QUIT, GameFocus, HomeMenu, Phase, Row, Setup};

/// Where the game is while it was sent to the launcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Away {
    /// In front.
    Playing,
    /// Leaving full screen before it hides.
    Leaving,
    /// Paused and hidden.
    Hidden,
}

struct Ui {
    menu: HomeMenu,
    setup: Setup,
    events: Arc<dyn AppEvents>,
    window: Retained<NSWindow>,
    overlay: Retained<NSView>,
    hold: Option<(Retained<NSView>, Retained<HoldRing>)>,
    shown: Option<(Retained<NSView>, Phase, usize)>,
    away: Away,
    /// The window was in full screen when it left: it goes back on resume.
    was_full: bool,
    /// A resumed window still owes its return to full screen.
    want_full: bool,
    observer: Retained<Observer>,
    start: Instant,
}

thread_local! {
    // Main thread only: AppKit calls every entry point there. A call that comes while another one runs (AppKit
    // reports a focus change inside our own `hide:`) is skipped.
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

fn with_ui(f: impl FnOnce(&mut Ui, MainThreadMarker)) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    UI.with(|cell| {
        if let Ok(mut ui) = cell.try_borrow_mut()
            && let Some(ui) = ui.as_mut()
        {
            f(ui, mtm);
        }
    });
}

define_class!(
    // SAFETY:
    // - `NSObject` has no subclassing requirements.
    // - `MainThreadOnly` is correct: the notifications and the delayed calls are delivered on the main thread.
    // - `Observer` does not implement `Drop`.
    /// Gets the notifications and the delayed calls of the Home button.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NxkitHomeObserver"]
    struct Observer;

    // SAFETY: `NSObjectProtocol` has no extra requirements.
    unsafe impl NSObjectProtocol for Observer {}

    impl Observer {
        #[unsafe(method(nxkitHomeResume:))]
        fn resume_asked(&self, _n: &NSNotification) {
            with_ui(|ui, mtm| {
                if ui.away == Away::Playing {
                    eprintln!("nxkit-host: the launcher asked to resume, the game is in front");
                    bring_forward(ui, mtm);
                } else {
                    resume(ui, mtm, "the launcher asked");
                }
            });
        }

        #[unsafe(method(nxkitHomeQuit:))]
        fn quit_asked(&self, _n: &NSNotification) {
            with_ui(|ui, mtm| {
                eprintln!("nxkit-host: the launcher asked to quit");
                ui.events.quit("the launcher asked to quit");
                if ui.away != Away::Playing {
                    // A hidden window may get no display tick, so the tick cannot end the application.
                    NSApplication::sharedApplication(mtm).terminate(None);
                }
            });
        }

        #[unsafe(method(nxkitHomeDidExitFullScreen:))]
        fn did_exit_full_screen(&self, _n: &NSNotification) {
            with_ui(|ui, mtm| {
                observe_exit(ui, false);
                match ui.away {
                    Away::Leaving => hide(ui, mtm),
                    Away::Playing if ui.want_full => later(&ui.observer, sel!(nxkitHomeFullScreen:), 0.25),
                    _ => {}
                }
            });
        }

        #[unsafe(method(nxkitHomeHide:))]
        fn hide_later(&self, _o: Option<&AnyObject>) {
            // A refused exit from full screen does not keep the game on the screen.
            with_ui(|ui, mtm| {
                if ui.away == Away::Leaving {
                    observe_exit(ui, false);
                    hide(ui, mtm);
                }
            });
        }

        #[unsafe(method(nxkitHomeFullScreen:))]
        fn full_screen_later(&self, _o: Option<&AnyObject>) {
            with_ui(|ui, _| {
                if ui.away == Away::Playing && ui.want_full {
                    ui.want_full = false;
                    if !ui.window.styleMask().contains(NSWindowStyleMask::FullScreen) {
                        eprintln!("nxkit-host: back to full screen");
                        ui.window.toggleFullScreen(None);
                    }
                }
            });
        }
    }
);

impl Observer {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        // SAFETY: `init` of `NSObject` on a fresh instance.
        unsafe { msg_send![Self::alloc(mtm), init] }
    }
}

/// Calls `selector` (one object argument, given nil) of `observer` after `delay` seconds.
fn later(observer: &Observer, selector: objc2::runtime::Sel, delay: f64) {
    let none: Option<&AnyObject> = None;
    // SAFETY: `performSelector:withObject:afterDelay:` of `NSObject` on the main thread. Every selector passed here is
    // a method of `Observer` with one object argument.
    unsafe {
        let _: () =
            msg_send![observer, performSelector: selector, withObject: none, afterDelay: delay];
    }
}

/// Watches the window's exit from full screen (`on`), or stops.
fn observe_exit(ui: &Ui, on: bool) {
    let center = NSNotificationCenter::defaultCenter();
    // SAFETY: the observer implements the selector with one `NSNotification` argument and lives as long as the UI.
    // `removeObserver:name:object:` takes it off again.
    unsafe {
        let name = NSWindowDidExitFullScreenNotification;
        if on {
            center.addObserver_selector_name_object(
                &ui.observer,
                sel!(nxkitHomeDidExitFullScreen:),
                Some(name),
                Some(&ui.window),
            );
        } else {
            center.removeObserver_name_object(&ui.observer, Some(name), Some(&ui.window));
        }
    }
}

/// Puts the Home button on `window`. The screens go over `overlay`, the window's content view above the game.
pub(super) fn install(
    mtm: MainThreadMarker,
    window: &NSWindow,
    overlay: &NSView,
    events: Arc<dyn AppEvents>,
    setup: Setup,
) {
    let observer = Observer::new(mtm);
    let center = NSDistributedNotificationCenter::defaultCenter();
    // SAFETY: the observer implements both selectors with one `NSNotification` argument. It lives in the UI until
    // `shutdown` takes it off.
    unsafe {
        center.addObserver_selector_name_object(
            &observer,
            sel!(nxkitHomeResume:),
            Some(&notify::name("resume")),
            None,
        );
        center.addObserver_selector_name_object(
            &observer,
            sel!(nxkitHomeQuit:),
            Some(&notify::name("quit")),
            None,
        );
    }
    let o = setup.options;
    eprintln!(
        "nxkit-host: Home button: menu {}, hold to quit {}, back to the launcher {}; launcher requests \
         org.nxkit.resume.{pid} and org.nxkit.quit.{pid}",
        if o.menu {
            "on"
        } else {
            "off (NXKIT_HOME_MENU=0)"
        },
        if o.hold {
            "on"
        } else {
            "off (NXKIT_HOME_QUIT=0)"
        },
        setup
            .launcher
            .as_deref()
            .unwrap_or("off (no NXKIT_LAUNCHER_BUNDLE_ID)"),
        pid = std::process::id(),
    );
    let ui = Ui {
        menu: HomeMenu::new(o),
        setup,
        events,
        window: window.retain(),
        overlay: overlay.retain(),
        hold: None,
        shown: None,
        away: Away::Playing,
        was_full: false,
        want_full: false,
        observer,
        start: Instant::now(),
    };
    UI.with(|cell| *cell.borrow_mut() = Some(ui));
}

/// The application ends: the observers go.
pub(super) fn shutdown() {
    with_ui(|ui, _| {
        // SAFETY: removes the observer from every name it was added for.
        unsafe { NSDistributedNotificationCenter::defaultCenter().removeObserver(&ui.observer) };
        observe_exit(ui, false);
    });
}

/// The display tick: one step of the Home button.
pub(super) fn tick() {
    with_ui(|ui, mtm| {
        if !ui.menu.enabled() || ui.away != Away::Playing || ui.menu.phase() == Phase::Paused {
            return;
        }
        let active = NSApplication::sharedApplication(mtm).isActive();
        if !active || !ui.window.isKeyWindow() {
            close(ui);
            return;
        }
        let now = ui.start.elapsed().as_secs_f64();
        let was = ui.menu.phase();
        let phase = ui.menu.step(now, ui.events.home_buttons());
        if phase != was {
            changed(ui, mtm, was, phase);
        }
        show_screen(ui, mtm);
        match ui.menu.ring(now) {
            Some(fill) => {
                let (_, ring) = ui.hold.get_or_insert_with(|| {
                    let (view, ring) =
                        homeviews::hold(mtm, ui.overlay.bounds(), &ui.setup.texts.hold);
                    ui.overlay.addSubview(&view);
                    homeviews::fade_in(&view);
                    (view, ring)
                });
                ring.set(fill);
            }
            None => {
                if let Some((view, _)) = ui.hold.take() {
                    view.removeFromSuperview();
                }
            }
        }
    });
}

/// The window lost the key focus (another app, the system's own use of Home): whatever was shown closes.
pub(super) fn focus_lost() {
    with_ui(|ui, _| close(ui));
}

/// The application is active or shown again: a game paused in the background resumes.
pub(super) fn app_active() {
    with_ui(|ui, mtm| {
        if ui.away == Away::Hidden {
            resume(ui, mtm, "the game is in front again");
        }
    });
}

fn close(ui: &mut Ui) {
    let was = ui.menu.phase();
    if matches!(was, Phase::Idle | Phase::Quit | Phase::Paused) {
        return;
    }
    ui.menu.cancel();
    if let Some((view, _)) = ui.hold.take() {
        view.removeFromSuperview();
    }
    hide_screen(ui, true);
    if matches!(was, Phase::Menu | Phase::Prompt | Phase::Settle) {
        eprintln!("nxkit-host: Home screen closed, the window lost the focus");
        ui.events.game_focus(GameFocus::In);
        ui.events.home_input(false);
    }
}

fn changed(ui: &mut Ui, mtm: MainThreadMarker, was: Phase, now: Phase) {
    let open = |p: Phase| matches!(p, Phase::Menu | Phase::Prompt);
    match now {
        Phase::Menu | Phase::Prompt if !open(was) => {
            ui.events.home_input(true);
            ui.events.game_focus(GameFocus::Out);
            super::mouse::release();
            eprintln!(
                "nxkit-host: {}",
                if now == Phase::Menu {
                    "Home pressed, Home menu open"
                } else {
                    "Home held, quit prompt open"
                }
            );
        }
        Phase::Settle => {
            hide_screen(ui, true);
            ui.events.game_focus(GameFocus::In);
            eprintln!("nxkit-host: Home screen closed, the game goes on");
        }
        Phase::Idle if was == Phase::Settle => ui.events.home_input(false),
        Phase::Quit => {
            hide_screen(ui, false);
            eprintln!("nxkit-host: quit from the Home button");
            ui.events.quit("quit from the Home button");
        }
        Phase::Paused => {
            hide_screen(ui, false);
            go_away(ui, mtm);
        }
        _ => {}
    }
}

/// The menu or the prompt as the state is now, built again when its focus moved.
fn show_screen(ui: &mut Ui, mtm: MainThreadMarker) {
    let phase = ui.menu.phase();
    let focus = match phase {
        Phase::Menu => ui.menu.selected(),
        Phase::Prompt => ui.menu.answer(),
        _ => return,
    };
    if ui
        .shown
        .as_ref()
        .is_some_and(|(_, p, f)| *p == phase && *f == focus)
    {
        return;
    }
    let view = build(ui, mtm, phase, focus);
    let fresh = ui.shown.as_ref().is_none_or(|(_, p, _)| *p != phase);
    hide_screen(ui, fresh);
    ui.overlay.addSubview(&view);
    if fresh {
        homeviews::fade_in(&view);
    }
    ui.shown = Some((view, phase, focus));
}

fn build(ui: &Ui, mtm: MainThreadMarker, phase: Phase, focus: usize) -> Retained<NSView> {
    let t = &ui.setup.texts;
    let rows: Vec<(&str, bool)> = ui
        .menu
        .rows()
        .iter()
        .map(|r| match r {
            Row::Resume => (t.resume.as_str(), true),
            Row::Launcher => (t.launcher.as_str(), false),
            Row::Quit => (t.quit.as_str(), false),
        })
        .collect();
    let answers = [(t.cancel.as_str(), false), (t.quit.as_str(), true)];
    let card = if phase == Phase::Menu {
        Card {
            symbol: "house",
            title: &t.menu_title,
            message: None,
            actions: &rows,
            rows: true,
            focused: focus,
        }
    } else {
        Card {
            symbol: "xmark.circle",
            title: &t.title,
            message: Some(&t.note),
            actions: &answers,
            rows: false,
            focused: focus.min(ANSWER_QUIT),
        }
    };
    homeviews::card(mtm, ui.overlay.bounds(), &card)
}

fn hide_screen(ui: &mut Ui, fade: bool) {
    if let Some((view, _, _)) = ui.shown.take() {
        if fade {
            homeviews::fade_out(&view);
        } else {
            view.removeFromSuperview();
        }
    }
}

fn go_away(ui: &mut Ui, mtm: MainThreadMarker) {
    eprintln!("nxkit-host: Home menu: back to the launcher, pausing the game");
    ui.events.game_focus(GameFocus::Background);
    super::mouse::release();
    let full = ui
        .window
        .styleMask()
        .contains(NSWindowStyleMask::FullScreen);
    ui.was_full = full || ui.want_full;
    ui.want_full = false;
    ui.away = Away::Leaving;
    if full {
        observe_exit(ui, true);
        later(&ui.observer, sel!(nxkitHomeHide:), 3.0);
        ui.window.toggleFullScreen(None);
    } else {
        hide(ui, mtm);
    }
}

fn hide(ui: &mut Ui, mtm: MainThreadMarker) {
    if ui.away != Away::Leaving {
        return;
    }
    let app = NSApplication::sharedApplication(mtm);
    activate_launcher(ui, &app);
    ui.away = Away::Hidden;
    app.hide(None);
    eprintln!("nxkit-host: paused in the background");
    notify::post("paused");
}

/// Hands the front to the launcher: its process when it gave it (two copies of a launcher may run), else the first
/// application with its bundle identifier.
fn activate_launcher(ui: &Ui, app: &NSApplication) {
    let s = &ui.setup;
    let by_pid = s
        .launcher_pid
        .and_then(NSRunningApplication::runningApplicationWithProcessIdentifier);
    let launcher = by_pid.or_else(|| {
        let id = NSString::from_str(s.launcher.as_deref()?);
        NSRunningApplication::runningApplicationsWithBundleIdentifier(&id).firstObject()
    });
    let name = match (s.launcher_pid, s.launcher.as_deref()) {
        (Some(pid), _) => format!("process {pid}"),
        (None, Some(id)) => id.to_owned(),
        (None, None) => {
            eprintln!("nxkit-host: background: no launcher, macOS shows the app behind the game");
            return;
        }
    };
    match launcher {
        Some(launcher) => {
            app.yieldActivationToApplication(&launcher);
            let me = NSRunningApplication::currentApplication();
            let ok = launcher.activateFromApplication_options(
                &me,
                NSApplicationActivationOptions::ActivateAllWindows,
            );
            eprintln!(
                "nxkit-host: background: launcher {name} {}",
                if ok {
                    "activated"
                } else {
                    "did not take the activation"
                }
            );
        }
        None => eprintln!("nxkit-host: background: launcher {name} is not running"),
    }
}

fn bring_forward(ui: &Ui, mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    app.unhide(None);
    ui.window.makeKeyAndOrderFront(None);
    app.activate();
}

fn resume(ui: &mut Ui, mtm: MainThreadMarker, why: &str) {
    if ui.away == Away::Playing {
        return;
    }
    let leaving = ui.away == Away::Leaving;
    ui.away = Away::Playing;
    ui.want_full = ui.was_full;
    eprintln!("nxkit-host: resuming ({why})");
    ui.events.game_focus(GameFocus::In);
    ui.menu.resumed();
    bring_forward(ui, mtm);
    // While the window still leaves full screen, the exit notification sends it back.
    if ui.want_full && !leaving {
        later(&ui.observer, sel!(nxkitHomeFullScreen:), 0.25);
    }
    eprintln!("nxkit-host: resumed");
    notify::post("resumed");
}
