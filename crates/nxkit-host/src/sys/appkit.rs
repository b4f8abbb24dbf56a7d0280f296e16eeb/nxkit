//! The window: `NSApplication` on the main thread, one window, a `CAMetalLayer`, the presenter and the display link.
//!
//! The module is in `sys` because AppKit calls and the delegate class need `unsafe`.

use std::cell::OnceCell;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use nxkit_metal::{Context, DisplayLink, DisplayTick, FrameHooks, FrameInterval, Presenter};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{Ivars, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType, NSEvent, NSResponder,
    NSScreen, NSView, NSWindow, NSWindowCollectionBehavior, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSNotification, NSObject, NSObjectProtocol, NSString};
use objc2_quartz_core::{CALayer, CAMetalLayer};

use super::mouse;
use crate::error::{HostError, Result};

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
}

/// The screen of the window: `NXKIT_SCREEN` names it by its index in the screen list (0 is the built-in one) or by a part
/// of its name, for example `XB273U`. Without it, or when nothing matches, it is the main screen.
fn target_screen(mtm: MainThreadMarker) -> Option<Retained<NSScreen>> {
    if let Ok(want) = std::env::var("NXKIT_SCREEN") {
        let screens = NSScreen::screens(mtm);
        let found = match want.parse::<usize>() {
            Ok(i) if i < screens.count() => Some(screens.objectAtIndex(i)),
            Ok(_) => None,
            Err(_) => {
                let want = want.to_lowercase();
                screens.iter().find(|s| s.localizedName().to_string().to_lowercase().contains(&want))
            }
        };
        match found {
            Some(s) => return Some(s),
            None => eprintln!("nxkit-host: NXKIT_SCREEN={want} matches no screen, using the main screen"),
        }
    }
    NSScreen::mainScreen(mtm)
}

/// The display link rate that the main screen supports. The game's own pacing (the title's frame step) picks the
/// frames it renders. The link never asks for less than the screen can show.
fn screen_interval(mtm: MainThreadMarker) -> FrameInterval {
    let screen = target_screen(mtm);
    if let Some(s) = &screen {
        nxkit_svc_pacing::set_screen_hz(u32::try_from(s.maximumFramesPerSecond()).unwrap_or(0));
    }
    match screen {
        Some(s) if s.maximumFramesPerSecond() >= 120 => FrameInterval::Hz120,
        _ => FrameInterval::Hz60,
    }
}

fn content_size(mtm: MainThreadMarker, size: Option<(f64, f64)>) -> (f64, f64) {
    if let Some(s) = size {
        return s;
    }
    match target_screen(mtm) {
        Some(s) => {
            let f = s.visibleFrame();
            (f.size.width, f.size.height)
        }
        None => (1280.0, 720.0),
    }
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
            mouse::focus(false);
            if let Some(e) = self.events().get() {
                e.focus_changed(false);
            }
        }

        #[unsafe(method(windowDidResize:))]
        fn window_did_resize(&self, _n: &NSNotification) {
            if let (Some(p), Some(w), Some(v)) =
                (self.presenter().get(), self.window().get(), self.view().get())
            {
                let (width, height) = drawable_size(w, v);
                p.resize(width, height);
            }
        }
    }

    // SAFETY: the methods have the signatures of the protocol.
    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _app: &NSApplication) -> bool {
            true
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn application_will_terminate(&self, _n: &NSNotification) {
            mouse::release();
            if let Some(e) = self.events().get() {
                e.closing();
            }
        }
    }
);

/// Passes every hook to the host and ends the application on the next tick after a stop request. The tick is the
/// only code that runs on the main thread by itself, so the stop request waits for it.
struct StopHooks {
    inner: Arc<dyn FrameHooks>,
    stop: Arc<AtomicBool>,
}

impl FrameHooks for StopHooks {
    fn frame_begin(&self, frame: u64) {
        self.inner.frame_begin(frame);
    }

    fn before_present(&self, frame: u64) {
        self.inner.before_present(frame);
    }

    fn frame_complete(&self, stats: &nxkit_metal::CommandBufferStats) {
        self.inner.frame_complete(stats);
    }

    fn display_tick(&self, tick: DisplayTick) {
        if self.stop.load(Ordering::Acquire) {
            if let Some(mtm) = MainThreadMarker::new() {
                NSApplication::sharedApplication(mtm).terminate(None);
            }
            return;
        }
        mouse::tick();
        self.inner.display_tick(tick);
    }
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
    events: Arc<dyn AppEvents>,
    stop: Arc<AtomicBool>,
    ready: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let mtm = MainThreadMarker::new().ok_or_else(|| HostError::App("the window must start on the main thread".into()))?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    let (width, height) = content_size(mtm, cfg.size);
    let rect = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(width, height));
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
    // SAFETY: the window is kept in `Retained` and `AppDelegate`. AppKit must not release it again on close.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str(&cfg.title));
    window.setCollectionBehavior(NSWindowCollectionBehavior::FullScreenPrimary);

    let game_view = GameView::new(mtm, rect);
    let view: Retained<NSView> = Retained::into_super(game_view);
    let layer = CAMetalLayer::new();
    let as_layer: &CALayer = &layer;
    view.setLayer(Some(as_layer));
    view.setWantsLayer(true);
    window.setContentView(Some(&view));
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
    let delegate = AppDelegate::alloc(mtm).set_ivars(Ivars::<AppDelegate> {
        events: OnceCell::from(events),
        presenter: OnceCell::from(presenter.clone()),
        window: OnceCell::from(window.clone()),
        view: OnceCell::from(view.clone()),
    });
    // SAFETY: `init` of `NSObject` on a freshly allocated instance whose ivars are set.
    let delegate: Retained<AppDelegate> = unsafe { msg_send![super(delegate), init] };
    // SAFETY: the delegate is kept alive by `delegate` for the whole run of the app, and the window only holds it weakly.
    unsafe { window.setDelegate(Some(ProtocolObject::from_ref(&*delegate))) };
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

    let link_hooks: Arc<dyn FrameHooks> = Arc::new(StopHooks { inner: hooks, stop });
    let _link = DisplayLink::new(mtm, &presenter, screen_interval(mtm), link_hooks);

    match target_screen(mtm) {
        Some(screen) if std::env::var_os("NXKIT_SCREEN").is_some() => {
            let (v, f) = (screen.visibleFrame(), window.frame());
            let x = v.origin.x + (v.size.width - f.size.width).max(0.0) / 2.0;
            let y = v.origin.y + (v.size.height - f.size.height).max(0.0) / 2.0;
            window.setFrameOrigin(CGPoint::new(x, y));
        }
        _ => window.center(),
    }
    window.makeKeyAndOrderFront(None);
    if cfg.fullscreen {
        window.toggleFullScreen(None);
    }
    app.activate();

    ready()?;
    app.run();
    Ok(())
}
