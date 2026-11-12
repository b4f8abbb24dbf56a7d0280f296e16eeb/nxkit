//! One process-lifetime game, connected to UIKit and the real nxkit kernel/GPU.
use crate::api::{Paths, Session, Touch};
use crate::{
    HostError, Result, boot::Boot, bundle, guestsym, kernel_map, loader, pipecache, place, refresh,
    seams, stop, sys, thread,
};
use nxkit_metal::{Context, DisplayLink, Presenter};
use objc2::{MainThreadMarker, rc::Retained};
use objc2_quartz_core::CAMetalLayer;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
static STARTED: AtomicBool = AtomicBool::new(false);

struct GameSession {
    // A dylib is deliberately not unloaded: the shared kernel has detached guest
    // threads and process-lifetime OnceLocks. Relaunch, rather than in-process reset.
    _loaded: loader::Loaded,
    _memory: Arc<nxkit_metal::GuestMemory>,
    presenter: Presenter,
    link: DisplayLink,
    stopped: bool,
}
impl Session for GameSession {
    fn set_active(&mut self, active: bool) {
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
            eprintln!("nx2ipa audio lifecycle: {error}");
        }
    }
    fn resize(&mut self, width: u32, height: u32) {
        if !self.stopped {
            self.presenter.resize(width, height);
        }
    }
    fn touches(&mut self, points: &[Touch]) {
        if !self.stopped {
            let points: Vec<_> = points
                .iter()
                .map(|p| nxkit_svc_input::Touch {
                    id: p.id,
                    x: p.x,
                    y: p.y,
                })
                .collect();
            nxkit_svc_input::set_touches(&points);
        }
    }
    fn virtual_pad(&mut self, buttons: u64, lx: f32, ly: f32, rx: f32, ry: f32) {
        if !self.stopped && !stop::requested() {
            nxkit_svc_input::set_virtual_pad(buttons, lx, ly, rx, ry);
        }
    }
    fn has_controller(&self) -> bool {
        !self.stopped && nxkit_svc_input::has_controller()
    }
    fn is_handheld(&self) -> bool {
        nxkit_svc_input::is_handheld()
    }
    fn stop(&mut self) {
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
pub fn start(
    layer: Retained<CAMetalLayer>,
    paths: Paths,
    maximum_fps: u32,
) -> std::result::Result<Box<dyn Session>, String> {
    start_checked(layer, paths, maximum_fps).map_err(|e| e.to_string())
}
fn start_checked(
    layer: Retained<CAMetalLayer>,
    paths: Paths,
    maximum_fps: u32,
) -> Result<Box<dyn Session>> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| HostError::App("Game session requires UIKit's main thread".into()))?;
    if STARTED.swap(true, Ordering::AcqRel) {
        return Err(HostError::App(
            "A game already initialized this process; relaunch to restart".into(),
        ));
    }
    eprintln!("nxkit-launch: reading game manifest");
    let game = bundle::read(&paths.bundle, &paths.data)?;
    let manifest_path = paths.bundle.join("game.json");
    let manifest = std::fs::read(&manifest_path).map_err(|e| HostError::io(&manifest_path, &e))?;
    pipecache::configure(&paths.cache.join(&game.title_id), &manifest)?;
    let mask = loader::service_mask(&game.services)?;
    if mask & nxkit_rt::Service::Kernel.bit() == 0 {
        return Err(HostError::Manifest("services must include kernel".into()));
    }
    // Establish foreground=false before guest audio can open an output.
    nxkit_svc_audio::set_active(false).map_err(HostError::App)?;
    eprintln!("nxkit-launch: placing guest memory");
    let placed = place::place(&game)?;
    let mem = placed.mem;
    let layout = mem.layout;
    let settings = placed.game.settings.clone();
    // The unchanged kernel ABI assumes contiguous sections within each record.
    // Supply disjoint records so real NSO offsets and widened data are preserved.
    let kernel_game = kernel_map::configuration(&placed.game)?;
    eprintln!("nxkit-launch: creating Metal context");
    let ctx = Context::system_default().map_err(|e| HostError::App(format!("Metal: {e}")))?;
    eprintln!(
        "nx2ipa: guest GPU span {} bytes; device buffer limit {} bytes",
        mem.gpu_range().len,
        ctx.max_buffer_length()
    );
    eprintln!("nxkit-launch: mapping guest GPU memory");
    let memory = Arc::new(mem.gpu_memory(&ctx)?);
    mem.seal()?;
    nxkit_rt::nxkit_init(mask);
    eprintln!("nxkit-launch: initializing kernel");
    sys::kernel_init(thread::guest_thread_entry, &layout, &kernel_game)?;
    seams::register_kernel()?;
    seams::register_server(mask)?;
    eprintln!("nxkit-launch: initializing services");
    seams::init_services(&placed.game, mask)?;
    eprintln!("nxkit-launch: loading game modules");
    let mut loaded = loader::load(placed, mask)?;
    guestsym::set(std::mem::take(&mut loaded.exports));
    eprintln!("nxkit-launch: resolving guest boot");
    let boot = Boot::resolve(std::mem::take(&mut loaded.inits))?;
    refresh::configure(refresh::Request::from_settings(&settings));
    let rate = refresh::apply(maximum_fps);
    eprintln!("nxkit-launch: creating frame hooks");
    let hooks = seams::frame_hooks(&ctx, &memory, &paths.bundle.join("nxkit"), &settings)?;
    let presenter = sys::ios::presenter(&ctx, &layer)
        .map_err(|e| HostError::App(format!("Metal presentation: {e}")))?;
    eprintln!("nxkit-launch: creating display link");
    let link = DisplayLink::new(mtm, &presenter, rate, hooks);
    link.set_paused(true);
    let session = GameSession {
        _loaded: loaded,
        _memory: memory,
        presenter,
        link,
        stopped: false,
    };
    eprintln!("nxkit-launch: starting guest main thread");
    thread::start_main(boot, layout.main_stack.end())?;
    Ok(Box::new(session))
}
