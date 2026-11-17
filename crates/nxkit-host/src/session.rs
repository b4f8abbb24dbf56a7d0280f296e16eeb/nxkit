//! One process-lifetime game, connected to UIKit and the real nxkit kernel/GPU.
use crate::api::{Paths, Touch};
use crate::{
    Game, HostError, Result, loader, pipecache, refresh, seams, startup, stop, sys, thread,
};
use nxkit_metal::{DisplayLink, Presenter};
use objc2::{MainThreadMarker, rc::Retained};
use objc2_quartz_core::CAMetalLayer;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
static STARTED: AtomicBool = AtomicBool::new(false);

pub(crate) struct GameSession {
    // A dylib is deliberately not unloaded: the shared kernel has detached guest
    // threads and process-lifetime OnceLocks. Relaunch, rather than in-process reset.
    _loaded: loader::Loaded,
    _memory: Arc<nxkit_metal::GuestMemory>,
    presenter: Presenter,
    link: DisplayLink,
    stopped: bool,
}
impl GameSession {
    pub(crate) fn set_active(&mut self, active: bool) {
        let active = active && !self.stopped && !stop::requested();
        self.link.set_paused(!active);
        nxkit_svc_input::set_focus(active);
        if !active {
            nxkit_svc_input::set_touches(&[]);
        }
        nxkit_svc_system::set_focus(if active {
            nxkit_svc_system::Focus::InFocus
        } else {
            nxkit_svc_system::Focus::OutOfFocus
        });
        if let Err(error) = nxkit_svc_audio::set_active(active) {
            eprintln!("nxkit audio lifecycle: {error}");
        }
    }
    pub(crate) fn resize(&mut self, width: u32, height: u32) {
        if !self.stopped {
            self.presenter.resize(width, height);
        }
    }
    pub(crate) fn touches(&mut self, points: &[Touch]) {
        if !self.stopped {
            nxkit_svc_input::set_touches(points);
        }
    }
    pub(crate) fn virtual_pad(&mut self, buttons: u64, lx: f32, ly: f32, rx: f32, ry: f32) {
        if !self.stopped && !stop::requested() {
            nxkit_svc_input::set_virtual_pad(buttons, lx, ly, rx, ry);
        }
    }
    pub(crate) fn has_controller(&self) -> bool {
        !self.stopped && nxkit_svc_input::has_controller()
    }
    pub(crate) fn is_handheld(&self) -> bool {
        nxkit_svc_input::is_handheld()
    }
    pub(crate) fn stop(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        self.set_active(false);
        // No guest-memory teardown can be safe until all kernel threads have joined.
        // The shared runtime ends this single-game process; mappings remain registered.
        stop::request("the UIKit game scene disconnected");
    }
}

/// Factory called by UIKit on its main thread; starts guest execution separately.
pub(crate) fn start(
    layer: Retained<CAMetalLayer>,
    paths: Paths,
    maximum_fps: u32,
) -> std::result::Result<GameSession, String> {
    start_checked(layer, paths, maximum_fps).map_err(|e| e.to_string())
}
fn start_checked(
    layer: Retained<CAMetalLayer>,
    paths: Paths,
    maximum_fps: u32,
) -> Result<GameSession> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| HostError::App("Game session requires UIKit's main thread".into()))?;
    let game = Game::read(&paths.bundle)?;
    let mask = startup::service_mask(&game)?;
    if STARTED.swap(true, Ordering::AcqRel) {
        return Err(HostError::App(
            "A game already initialized this process; relaunch to restart".into(),
        ));
    }
    pipecache::configure(&paths.cache.join(&game.title_id))?;
    // Establish foreground=false before guest audio can open an output.
    nxkit_svc_audio::set_active(false).map_err(HostError::App)?;
    let startup::Prepared {
        loaded,
        memory,
        context: ctx,
        layout,
        boot,
    } = startup::prepare(&game, mask, &paths.data)?;
    let settings = loaded.game.settings.clone();
    refresh::configure(refresh::Request::from_settings(&settings));
    let rate = refresh::apply(maximum_fps);
    let hooks = seams::frame_hooks(&ctx, &memory, &paths.bundle, &settings)?;
    let presenter = sys::ios::presenter(&ctx, &layer)
        .map_err(|e| HostError::App(format!("Metal presentation: {e}")))?;
    let link = DisplayLink::new(mtm, &presenter, rate, hooks);
    link.set_paused(true);
    let session = GameSession {
        _loaded: loaded,
        _memory: memory,
        presenter,
        link,
        stopped: false,
    };
    thread::start_main(boot, layout.main_stack.end())?;
    Ok(session)
}
