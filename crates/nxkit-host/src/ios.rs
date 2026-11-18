//! UIKit owns the layer and controls; the shared host owns guest execution.
use crate::{Game, HostError, Result, loader, refresh, seams, startup, stop, thread};
use nxkit_metal::{DisplayLink, FrameInterval, LinkRate, Presenter};
use objc2::{MainThreadMarker, rc::Retained};
use objc2_quartz_core::CAMetalLayer;
use std::{
    cell::{Cell, RefCell},
    ffi::{CStr, c_char, c_void},
    path::Path,
    ptr::NonNull,
    sync::Arc,
};

struct Session {
    // Guest threads and global kernel state live until process exit.
    _loaded: loader::Loaded,
    _memory: Arc<nxkit_metal::GuestMemory>,
    link: DisplayLink,
}
thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
    static STARTED: Cell<bool> = const { Cell::new(false) };
}
fn with_session(f: impl FnOnce(&mut Session)) {
    if MainThreadMarker::new().is_some() {
        SESSION.with_borrow_mut(|s| {
            if let Some(s) = s {
                f(s);
            }
        });
    }
}
fn start(
    layer: Retained<CAMetalLayer>,
    bundle: &Path,
    home: &Path,
    maximum_fps: u32,
) -> Result<Session> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| HostError::App("UIKit main thread required".into()))?;
    let game = Game::read(bundle)?;
    let mask = startup::service_mask(&game)?;
    if STARTED.replace(true) {
        return Err(HostError::App(
            "Restart the app to start another game".into(),
        ));
    }
    let data = home.join("Library/Application Support/nxkit");
    nxkit_svc_audio::set_active(false).map_err(HostError::App)?;
    let startup::Prepared {
        loaded,
        memory,
        context,
        layout,
        boot,
    } = startup::prepare(&game, mask, &data)?;
    let settings = &loaded.game.settings;
    let plan = refresh::apply(&refresh::ScreenFacts {
        name: "UIKit".into(),
        display: None,
        max_fps: maximum_fps.max(1),
        adaptive: false,
    });
    let hooks = seams::frame_hooks(&context, &memory, bundle, settings)?;
    // SAFETY: UIKit supplied a live CAMetalLayer, retained above and by Presenter.
    let presenter = unsafe { Presenter::from_raw_layer(&context, NonNull::from(&*layer).cast()) }
        .map_err(|e| HostError::App(format!("Metal: {e}")))?;
    let link = DisplayLink::new(
        mtm,
        &presenter,
        LinkRate::Fixed(FrameInterval::from_hz(plan.link_hz)),
        hooks,
    );
    link.set_paused(true);
    nxkit_rt::nvn::set_active(false);
    let session = Session {
        _loaded: loaded,
        _memory: memory,
        link,
    };
    thread::start_main(boot, layout.main_stack.end())?;
    Ok(session)
}

#[unsafe(no_mangle)]
unsafe extern "C" fn nxkit_start(
    layer: *mut c_void,
    bundle: *const c_char,
    home: *const c_char,
    maximum_fps: u32,
    error: *mut c_char,
    capacity: usize,
) -> bool {
    let result = (|| {
        if MainThreadMarker::new().is_none()
            || layer.is_null()
            || bundle.is_null()
            || home.is_null()
        {
            return Err("Invalid UIKit session arguments".to_string());
        }
        // SAFETY: the Swift caller passes live NUL-terminated strings during this call.
        let bundle = unsafe { CStr::from_ptr(bundle) }
            .to_str()
            .map_err(|e| e.to_string())?;
        // SAFETY: same borrowed string contract for the process sandbox home.
        let home = unsafe { CStr::from_ptr(home) }
            .to_str()
            .map_err(|e| e.to_string())?;
        // SAFETY: UIKit owns this live CAMetalLayer; retain it before storing it.
        let layer = unsafe { Retained::retain(layer.cast::<CAMetalLayer>()) }
            .ok_or("Missing Metal layer")?;
        let session = start(layer, Path::new(bundle), Path::new(home), maximum_fps)
            .map_err(|e| e.to_string())?;
        SESSION.with_borrow_mut(|s| *s = Some(session));
        Ok::<(), String>(())
    })();
    if !error.is_null() && capacity > 0 {
        let message = result.as_ref().err().map_or("", String::as_str);
        let mut end = message.len().min(capacity - 1);
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        // SAFETY: caller provides capacity writable bytes; never copy past that bound.
        unsafe {
            std::ptr::copy_nonoverlapping(message.as_ptr(), error.cast(), end);
            *error.add(end) = 0;
        }
    }
    result.is_ok()
}
#[unsafe(no_mangle)]
extern "C" fn nxkit_set_active(active: bool) {
    with_session(|s| {
        let active = active && !stop::requested();
        if !active {
            s.link.set_paused(true);
        }
        seams::focus_changed(active);
        if !active {
            nxkit_svc_input::set_touches(&[]);
        }
        if let Err(error) = nxkit_svc_audio::set_active(active) {
            eprintln!("nxkit audio: {error}");
        }
        nxkit_rt::nvn::set_active(active);
        if active {
            s.link.set_paused(false);
        }
    });
}
#[unsafe(no_mangle)]
extern "C" fn nxkit_virtual_pad(buttons: u64, lx: f32, ly: f32, rx: f32, ry: f32) {
    with_session(|_| {
        if !stop::requested() {
            nxkit_svc_input::set_virtual_pad(buttons, lx, ly, rx, ry);
        }
    });
}
#[unsafe(no_mangle)]
unsafe extern "C" fn nxkit_touches(points: *const nxkit_svc_input::Touch, count: usize) {
    if count > 16 || (points.is_null() && count != 0) {
        return;
    }
    // SAFETY: UIKit passes count live Touch values for this call; empty needs no pointer.
    let points = if count == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(points, count) }
    };
    with_session(|_| nxkit_svc_input::set_touches(points));
}
