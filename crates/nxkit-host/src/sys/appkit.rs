//! The window: `NSApplication` on the main thread, one window, a `CAMetalLayer`, the presenter and the display link.
//!
//! The module is in `sys` because AppKit calls and the delegate class need `unsafe`.

use std::cell::{Cell, OnceCell};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use nxkit_metal::{Context, DisplayLink, FrameHooks, Presenter};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{Ivars, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSAutoresizingMaskOptions,
    NSBackingStoreType, NSEvent, NSResponder, NSView, NSWindow, NSWindowCollectionBehavior,
    NSWindowDelegate, NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{
    NSActivityOptions, NSNotification, NSObject, NSObjectProtocol, NSProcessInfo, NSString,
};
use objc2_quartz_core::{CALayer, CAMetalLayer};

use super::fullscreen::{self, Startup};
use super::homeui;
use super::mouse;
use super::present;
use super::screen::{content_size, screen_interval, target_screen};
use super::stophooks::StopHooks;
use crate::error::{HostError, Result};
use crate::refresh::Refresh;

/// What the window reports to the host. The calls come on the main thread.
pub trait AppEvents: Send + Sync {
    /// The window became the key window (`true`) or lost it (`false`).
    fn focus_changed(&self, focused: bool);
    /// The window closes or the application terminates. The game must stop.
    fn closing(&self);
    /// The mouse did something while the window holds it (`sys/mouse.rs`).
    fn mouse(&self, event: MouseEvent);
    /// When the window holds the mouse. The display tick asks, so the answer may change at any time.
    fn mouse_mode(&self) -> MouseMode;
    /// The buttons of every controller for the Home button (`sys/homeui.rs`).
    fn home_buttons(&self) -> crate::home::Buttons;
    /// A screen of the Home button has the controllers (`true`), or the game gets them back.
    fn home_input(&self, blocked: bool);
    /// The focus the game is told while a screen of the Home button is up.
    fn game_focus(&self, focus: crate::home::GameFocus);
    /// The player quit with the Home button, or the launcher asked. The game must stop.
    fn quit(&self, why: &'static str);
}

/// What the held mouse did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MouseEvent {
    /// It moved by `dx`, `dy` points, y down.
    Moved {
        /// To the right.
        dx: f64,
        /// Down.
        dy: f64,
    },
    /// A button went down or up. 0 is left, 1 right, 2 middle.
    Button {
        /// The AppKit button number.
        number: u8,
        /// Down or up.
        down: bool,
    },
    /// The wheel or the trackpad scrolled by `dy`, positive away from the user.
    Wheel {
        /// The scroll distance.
        dy: f64,
    },
    /// The window let go of the mouse. Every held button counts as up.
    Released,
}

/// When the window holds the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseMode {
    /// Never. The game gets no mouse.
    Off,
    /// After a click into the window, for example while a controller is connected.
    Click,
    /// Whenever the window is the key window, and after a click.
    Auto,
}

/// How the window looks.
#[derive(Debug, Clone)]
pub struct WindowConfig {
    /// The title.
    pub title: String,
    /// The size of the content area in points. `None` takes the visible area of the main screen, so that the layer
    /// has the pixels of the display and the game renders at its native size.
    pub size: Option<(f64, f64)>,
    /// Enter full screen at start.
    pub fullscreen: bool,
    /// The Metal performance HUD over the game (`developerHUDProperties` of the layer).
    pub metal_hud: bool,
    /// The Home button of the controllers (`home.rs`), `None` when it is off.
    pub home: Option<crate::home::Setup>,
    /// The GPU tells when the picture first shows something (`nxkit_gpu::when_shown`, an NVN game). Else the first
    /// finished frame counts.
    pub probed: bool,
}

fn drawable_size(window: &NSWindow, view: &NSView) -> (u32, u32) {
    let b = view.bounds();
    let scale = window.backingScaleFactor();
    let px = |v: f64| (v * scale).round().max(1.0) as u32;
    (px(b.size.width), px(b.size.height))
}

define_class!(
    // SAFETY:
    // - `NSView` has no subclassing requirements beyond calling its designated initialiser, which `GameView::new`
    //   does.
    // - `MainThreadOnly` is correct, because AppKit uses views on the main thread only.
    // - `GameView` does not implement `Drop`.
    /// The content view. The game reads the keyboard through GameController, so the view takes the key events of
    /// AppKit and drops them. Without it AppKit finds no responder for a key and plays the alert sound. The mouse
    /// events go to `sys/mouse.rs`, and Escape lets go of the mouse.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NxkitGameView"]
    struct GameView;

    impl GameView {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            mouse::key_down(event);
        }

        #[unsafe(method(keyUp:))]
        fn key_up(&self, _event: &NSEvent) {}

        #[unsafe(method(flagsChanged:))]
        fn flags_changed(&self, _event: &NSEvent) {}

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            mouse::moved(event);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            mouse::moved(event);
        }

        #[unsafe(method(rightMouseDragged:))]
        fn right_mouse_dragged(&self, event: &NSEvent) {
            mouse::moved(event);
        }

        #[unsafe(method(otherMouseDragged:))]
        fn other_mouse_dragged(&self, event: &NSEvent) {
            mouse::moved(event);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            mouse::button(event, true);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            mouse::button(event, false);
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            mouse::button(event, true);
        }

        #[unsafe(method(rightMouseUp:))]
        fn right_mouse_up(&self, event: &NSEvent) {
            mouse::button(event, false);
        }

        #[unsafe(method(otherMouseDown:))]
        fn other_mouse_down(&self, event: &NSEvent) {
            mouse::button(event, true);
        }

        #[unsafe(method(otherMouseUp:))]
        fn other_mouse_up(&self, event: &NSEvent) {
            mouse::button(event, false);
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            mouse::wheel(event);
        }
    }
);

impl GameView {
    /// A view of `frame`.
    fn new(mtm: MainThreadMarker, frame: CGRect) -> Retained<Self> {
        // SAFETY: `initWithFrame:` is the designated initialiser of `NSView`, called once on a fresh instance.
        unsafe { msg_send![Self::alloc(mtm), initWithFrame: frame] }
    }
}

define_class!(
    // SAFETY:
    // - `NSObject` has no subclassing requirements.
    // - `MainThreadOnly` is correct, because AppKit calls delegates on the main thread.
    // - `AppDelegate` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    struct AppDelegate {
        events: OnceCell<Arc<dyn AppEvents>>,
        presenter: OnceCell<Rc<Presenter>>,
        window: OnceCell<Retained<NSWindow>>,
        view: OnceCell<Retained<NSView>>,
        link: OnceCell<Rc<DisplayLink>>,
        refresh: OnceCell<Arc<Refresh>>,
        startup: Cell<Startup>,
    }

    // SAFETY: `NSObjectProtocol` has no extra requirements.
    unsafe impl NSObjectProtocol for AppDelegate {}

    // SAFETY: the methods have the signatures of the protocol.
    unsafe impl NSWindowDelegate for AppDelegate {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _n: &NSNotification) {
            mouse::release();
            if let Some(e) = self.events().get() {
                e.closing();
            }
        }

        #[unsafe(method(windowDidBecomeKey:))]
        fn window_did_become_key(&self, _n: &NSNotification) {
            if let Some(e) = self.events().get() {
                e.focus_changed(true);
            }
            mouse::focus(true);
        }

        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _n: &NSNotification) {
            homeui::focus_lost();
            mouse::focus(false);
            if let Some(e) = self.events().get() {
                e.focus_changed(false);
            }
        }

        #[unsafe(method(windowWillEnterFullScreen:))]
        fn window_will_enter_full_screen(&self, _n: &NSNotification) {
            self.with_startup(Startup::will_transition);
        }

        #[unsafe(method(windowWillExitFullScreen:))]
        fn window_will_exit_full_screen(&self, _n: &NSNotification) {
            self.with_startup(Startup::will_transition);
        }

        #[unsafe(method(windowDidEnterFullScreen:))]
        fn window_did_enter_full_screen(&self, _n: &NSNotification) {
            self.with_startup(Startup::entered);
        }

        #[unsafe(method(windowDidFailToEnterFullScreen:))]
        fn window_did_fail_to_enter_full_screen(&self, window: &NSWindow) {
            fullscreen::refused(MainThreadMarker::from(self), self, window, self.startup());
        }

        #[unsafe(method(windowDidChangeScreen:))]
        fn window_did_change_screen(&self, _n: &NSNotification) {
            self.replan();
        }

        #[unsafe(method(windowDidResize:))]
        fn window_did_resize(&self, _n: &NSNotification) {
            if let (Some(p), Some(w), Some(v)) = (
                self.presenter().get(),
                self.window().get(),
                self.view().get(),
            ) {
                let (width, height) = drawable_size(w, v);
                p.resize(width, height);
            }
        }
    }

    // SAFETY: the methods have the signatures of the protocol.
    unsafe impl NSApplicationDelegate for AppDelegate {
        // The activation asked for before the run loop started can get lost when a launcher hands the front over
        // (`yieldActivation`, cooperative activation): it is asked again once the application has launched.
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn application_did_finish_launching(&self, _n: &NSNotification) {
            let mtm = MainThreadMarker::from(self);
            // An overlay asks for the front only when it shows (`present.rs`): an active application with no window
            // on the launcher's Space would take the player to the desktop.
            if present::is_overlay() {
                return;
            }
            if let Some(w) = self.window().get() {
                w.makeKeyAndOrderFront(None);
            }
            NSApplication::sharedApplication(mtm).activate();
            self.enter_full_screen();
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _app: &NSApplication) -> bool {
            true
        }

        #[unsafe(method(applicationDidBecomeActive:))]
        fn application_did_become_active(&self, _n: &NSNotification) {
            self.enter_full_screen();
            homeui::app_active();
        }

        #[unsafe(method(applicationDidUnhide:))]
        fn application_did_unhide(&self, _n: &NSNotification) {
            homeui::app_active();
        }

        // A change of the display mode, the arrangement or the screens comes here.
        #[unsafe(method(applicationDidChangeScreenParameters:))]
        fn application_did_change_screen_parameters(&self, _n: &NSNotification) {
            self.replan();
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn application_will_terminate(&self, _n: &NSNotification) {
            nxkit_metal::display::restore_active();
            mouse::release();
            if let Some(e) = self.events().get() {
                e.closing();
            }
        }
    }

    impl AppDelegate {
        #[unsafe(method(nxkitEnterFullScreen))]
        fn enter_full_screen_later(&self) {
            self.enter_full_screen();
        }

        #[unsafe(method(nxkitActiveSpaceDidChange:))]
        fn active_space_did_change(&self, _n: &NSNotification) {
            self.enter_full_screen();
        }
    }
);

impl AppDelegate {
    /// Full screen at start, when the application is active and the window on the active Space (`fullscreen.rs`).
    fn enter_full_screen(&self) {
        if let Some(window) = self.window().get() {
            fullscreen::try_enter(MainThreadMarker::from(self), window, self.startup());
        }
    }

    fn with_startup(&self, f: impl FnOnce(&mut Startup)) {
        let mut s = self.startup().get();
        f(&mut s);
        self.startup().set(s);
    }

    /// The window moved to another screen, or the mode of a screen changed: read the refresh rate of the screen the
    /// window is on now, plan again and set the rate of the display link.
    fn replan(&self) {
        let (Some(window), Some(link), Some(refresh)) =
            (self.window().get(), self.link().get(), self.refresh().get())
        else {
            return;
        };
        let rate = screen_interval(MainThreadMarker::from(self), Some(window), refresh);
        if link.rate_range() != rate.range() {
            link.set_rate(rate);
        }
    }
}

/// The titled window of a game that has the desktop or its own full-screen Space.
fn titled_window(mtm: MainThreadMarker, rect: CGRect, title: &str) -> Retained<NSWindow> {
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    // SAFETY: a fresh `NSWindow` with valid arguments, on the main thread.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setTitle(&NSString::from_str(title));
    window.setCollectionBehavior(NSWindowCollectionBehavior::FullScreenPrimary);
    window
}

/// Opens the window and runs the application loop on the calling thread, which must be the main thread.
///
/// `ready` runs once the window and the display link exist, right before the loop starts. The game starts there.
/// `stop` is set by any thread that wants the application to end. The loop ends at the next display tick.
/// The function returns when the application has stopped.
pub fn run(
    cfg: &WindowConfig,
    ctx: &Context,
    hooks: Arc<dyn FrameHooks>,
    refresh: Arc<Refresh>,
    events: Arc<dyn AppEvents>,
    stop: Arc<AtomicBool>,
    ready: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| HostError::App("the window must start on the main thread".into()))?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    // `NXKIT_OVERLAY=1`: a borderless window over the launcher's full-screen Space (`present.rs`), else a titled one.
    let over = crate::hostenv::host()
        .overlay
        .then(|| target_screen(mtm).map(|s| s.frame()))
        .flatten();
    let (window, rect) = match over {
        Some(frame) => {
            let rect = CGRect::new(CGPoint::new(0.0, 0.0), frame.size);
            (present::overlay_window(mtm, frame), rect)
        }
        None => {
            let (width, height) = content_size(mtm, cfg.size);
            let rect = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(width, height));
            (titled_window(mtm, rect, &cfg.title), rect)
        }
    };
    // SAFETY: the window is kept in `Retained` and `AppDelegate`. AppKit must not release it again on close.
    unsafe { window.setReleasedWhenClosed(false) };
    present::black(&window);
    let fullscreen = cfg.fullscreen && over.is_none();

    let game_view = GameView::new(mtm, rect);
    let view: Retained<NSView> = Retained::into_super(game_view);
    let layer = CAMetalLayer::new();
    super::hud::show(&layer, cfg.metal_hud);
    let as_layer: &CALayer = &layer;
    view.setLayer(Some(as_layer));
    view.setWantsLayer(true);
    // With the Home button, the content view is a plain view that holds the game's view and, above it, the screens of
    // the Home button: a layer-hosting view must have no subviews of its own.
    let content = cfg.home.as_ref().map(|_| {
        let content = NSView::initWithFrame(NSView::alloc(mtm), rect);
        content.setWantsLayer(true);
        view.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        content.addSubview(&view);
        content
    });
    window.setContentView(Some(content.as_deref().unwrap_or(&view)));
    window.makeFirstResponder(Some(&view));
    window.setAcceptsMouseMovedEvents(true);

    let layer_ptr = NonNull::from(&*layer).cast::<c_void>();
    // SAFETY: `layer_ptr` points to the live `CAMetalLayer` above. The presenter retains it.
    let presenter = unsafe { Presenter::from_raw_layer(ctx, layer_ptr) }
        .map(Rc::new)
        .map_err(|e| HostError::App(format!("the presenter: {e}")))?;
    let (w, h) = drawable_size(&window, &view);
    presenter.resize(w, h);

    mouse::install(events.clone(), window.clone());
    let env = crate::hostenv::host();
    super::pointer::install(window.clone(), env.cursor_hide, env.cursor_hidden);
    if let (Some(setup), Some(content)) = (cfg.home.clone(), content.as_ref()) {
        super::textinputui::install(mtm, &window, content, events.clone(), setup.texts.clone());
        homeui::install(mtm, &window, content, events.clone(), setup);
    }
    present::install(window.clone(), over.is_some());
    let delegate = AppDelegate::alloc(mtm).set_ivars(Ivars::<AppDelegate> {
        events: OnceCell::from(events),
        presenter: OnceCell::from(presenter.clone()),
        window: OnceCell::from(window.clone()),
        view: OnceCell::from(view.clone()),
        link: OnceCell::new(),
        refresh: OnceCell::from(refresh.clone()),
        startup: Cell::new(Startup::new(fullscreen)),
    });
    // SAFETY: `init` of `NSObject` on a freshly allocated instance whose ivars are set.
    let delegate: Retained<AppDelegate> = unsafe { msg_send![super(delegate), init] };
    // SAFETY: the delegate is kept alive by `delegate` for the whole run of the app, and the window only holds it weakly.
    unsafe { window.setDelegate(Some(ProtocolObject::from_ref(&*delegate))) };
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

    let link_hooks: Arc<dyn FrameHooks> = Arc::new(StopHooks {
        inner: hooks,
        stop,
        probed: cfg.probed,
    });
    let link = Rc::new(DisplayLink::new(
        mtm,
        &presenter,
        screen_interval(mtm, None, &refresh),
        link_hooks,
    ));
    let _ = delegate.link().set(link);

    match target_screen(mtm) {
        _ if over.is_some() => {}
        Some(screen) if crate::hostenv::host().screen.is_some() => {
            let (v, f) = (screen.visibleFrame(), window.frame());
            let x = v.origin.x + (v.size.width - f.size.width).max(0.0) / 2.0;
            let y = v.origin.y + (v.size.height - f.size.height).max(0.0) / 2.0;
            window.setFrameOrigin(CGPoint::new(x, y));
        }
        _ => window.center(),
    }
    if over.is_none() {
        window.makeKeyAndOrderFront(None);
        app.activate();
    }
    if fullscreen {
        fullscreen::observe_spaces(&delegate, true);
        fullscreen::later(&delegate);
    }
    // The window is on its screen now, which may not be the one that was planned for.
    delegate.replan();

    // A game is latency critical and interactive whether its window is in front or not. Without the activity, macOS
    // treated the process as a background app while another window had the focus (App Nap, timer coalescing, a lower
    // scheduling class), and the frame rate fell. It also keeps the system from idle sleep while the game runs.
    let _activity = NSProcessInfo::processInfo().beginActivityWithOptions_reason(
        NSActivityOptions::UserInteractive,
        &NSString::from_str("a game runs"),
    );
    ready()?;
    app.run();
    super::textinputui::shutdown();
    homeui::shutdown();
    if fullscreen {
        fullscreen::observe_spaces(&delegate, false);
    }
    nxkit_metal::display::restore_active();
    Ok(())
}
