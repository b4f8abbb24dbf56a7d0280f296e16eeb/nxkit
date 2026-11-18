//! UIKit owns the layer and controls; the shared host owns guest execution.
use crate::{Game, HostError, Result, loader, refresh, seams, startup, stop, sys, thread};
use nxkit_metal::{DisplayLink, FrameInterval, LinkRate};
use objc2::{MainThreadMarker, rc::Retained};
use objc2_quartz_core::CAMetalLayer;
use std::{
    cell::{Cell, RefCell},
    path::Path,
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
pub(crate) fn start(
    layer: Retained<CAMetalLayer>,
    bundle: &Path,
    home: &Path,
    maximum_fps: u32,
) -> Result<()> {
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
    let presenter = sys::ios::presenter(&context, &layer)?;
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
    SESSION.with_borrow_mut(|s| *s = Some(session));
    Ok(())
}

pub(crate) fn set_active(active: bool) {
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
pub(crate) fn virtual_pad(buttons: u64, lx: f32, ly: f32, rx: f32, ry: f32) {
    with_session(|_| {
        if !stop::requested() {
            nxkit_svc_input::set_virtual_pad(buttons, lx, ly, rx, ry);
        }
    });
}
pub(crate) fn touches(points: &[nxkit_svc_input::Touch]) {
    with_session(|_| nxkit_svc_input::set_touches(points));
}
