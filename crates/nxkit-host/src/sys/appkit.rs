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
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType, NSScreen, NSView,
    NSWindow, NSWindowCollectionBehavior, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSNotification, NSObject, NSObjectProtocol, NSString};
use objc2_quartz_core::{CALayer, CAMetalLayer};

use crate::error::{HostError, Result};

/// What the window reports to the host. The calls come on the main thread.
pub trait AppEvents: Send + Sync {
    /// The window became the key window (`true`) or lost it (`false`).
    fn focus_changed(&self, focused: bool);
    /// The window closes or the application terminates. The game must stop.
    fn closing(&self);
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

/// The display link rate that the main screen supports. The game's own pacing (the title's frame step) picks the
/// frames it renders. The link never asks for less than the screen can show.
fn screen_interval(mtm: MainThreadMarker) -> FrameInterval {
    match NSScreen::mainScreen(mtm) {
        Some(s) if s.maximumFramesPerSecond() >= 120 => FrameInterval::Hz120,
        _ => FrameInterval::Hz60,
    }
}

fn content_size(mtm: MainThreadMarker, size: Option<(f64, f64)>) -> (f64, f64) {
    if let Some(s) = size {
        return s;
    }
    match NSScreen::mainScreen(mtm) {
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
            if let Some(e) = self.events().get() {
                e.closing();
            }
        }

        #[unsafe(method(windowDidBecomeKey:))]
        fn window_did_become_key(&self, _n: &NSNotification) {
            if let Some(e) = self.events().get() {
                e.focus_changed(true);
            }
        }

        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _n: &NSNotification) {
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

    let view = NSView::initWithFrame(NSView::alloc(mtm), rect);
    let layer = CAMetalLayer::new();
    let as_layer: &CALayer = &layer;
    view.setLayer(Some(as_layer));
    view.setWantsLayer(true);
    window.setContentView(Some(&view));

    let layer_ptr = NonNull::from(&*layer).cast::<c_void>();
    // SAFETY: `layer_ptr` points to the live `CAMetalLayer` above. The presenter retains it.
    let presenter = unsafe { Presenter::from_raw_layer(ctx, layer_ptr) }
        .map(Rc::new)
        .map_err(|e| HostError::App(format!("the presenter: {e}")))?;
    let (w, h) = drawable_size(&window, &view);
    presenter.resize(w, h);

    let delegate = AppDelegate::alloc(mtm).set_ivars(Ivars::<AppDelegate> {
        events: OnceCell::from(events),
        presenter: OnceCell::from(presenter.clone()),
        window: OnceCell::from(window.clone()),
        view: OnceCell::from(view.clone()),
    });
    // SAFETY: `init` of `NSObject` on a freshly allocated instance whose ivars are set.
    let delegate: Retained<AppDelegate> = unsafe { msg_send![super(delegate), init] };
    window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

    let link_hooks: Arc<dyn FrameHooks> = Arc::new(StopHooks { inner: hooks, stop });
    let _link = DisplayLink::new(mtm, &presenter, screen_interval(mtm), link_hooks);

    window.center();
    window.makeKeyAndOrderFront(None);
    if cfg.fullscreen {
        window.toggleFullScreen(None);
    }
    app.activate();

    ready()?;
    app.run();
    Ok(())
}
