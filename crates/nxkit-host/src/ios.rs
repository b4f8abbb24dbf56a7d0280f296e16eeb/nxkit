//! UIKit owns the layer and controls; the shared host owns guest execution.
use crate::{
    Game, HostError, Result, boot::Boot, hostenv::RunSettings, loader, place, refresh, seams, stop,
    sys, thread,
};
use nxkit_metal::Context;
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
    if STARTED.replace(true) {
        return Err(HostError::App(
            "Restart the app to start the game again".into(),
        ));
    }
    let mut run = RunSettings::from_env(&nxkit_abi::env::Env::capture());
    run.host.home = Some(home.join("Library/Application Support/nxkit"));
    run.host.save = None;
    run.install()
        .map_err(|e| HostError::Settings(e.to_string()))?;
    let game = Game::read(bundle, &run.host)?;
    if !matches!(game.graphics, crate::graphics::Graphics::Nvn) {
        return Err(HostError::Manifest(
            "The iOS shell expects an NVN game".into(),
        ));
    }
    let mask = loader::service_mask(&game.services)?;
    if mask & nxkit_rt::Service::Kernel.bit() == 0 {
        return Err(HostError::Manifest(
            "The game must include the kernel service".into(),
        ));
    }
    nxkit_svc_audio::set_active(false).map_err(HostError::App)?;
    let placed = place::place(&game)?;
    let mem = placed.mem;
    let layout = mem.layout;
    let settings = placed.game.settings.clone();
    nxkit_nvn::rendercond::set_title_stale(settings.rendercond_stale);
    let screen = Arc::new(nxkit_nvn::screen::Screen::new(Some(
        crate::frames::shown_to_step,
    )));
    let refresh = Arc::new(refresh::Refresh::new(
        refresh::Request::from_settings(&settings, &run.host)
            .at_120(seams::runs_at_120(&placed.game)),
        screen.clone(),
    ));
    let context = Context::system_default().map_err(|e| HostError::App(format!("Metal: {e}")))?;
    let memory = nxkit_gpu::GuestBuffer::Pools {
        space: mem.gpu_space()?,
        pins: Arc::new(crate::memory::KernelPins),
    };
    mem.seal()?;
    nxkit_rt::nxkit_init(mask);
    let arena = crate::obswire::arena_size(&placed.game, &run.ue, layout.alias.len)?;
    sys::kernel_init(thread::guest_thread_entry, &layout, &placed.game, arena)?;
    seams::register_kernel()?;
    seams::register_server(mask)?;
    seams::init_services(&placed.game, mask, &run.host, &refresh)?;
    let args = placed.game.program_args(settings.render_dims());
    let mut loaded = loader::load(placed, mask)?;
    crate::guestsym::set(std::mem::take(&mut loaded.exports));
    let boot = Boot::resolve(std::mem::take(&mut loaded.inits), &args)?;
    let plan = refresh.apply(&refresh::ScreenFacts {
        name: "UIKit".into(),
        display: None,
        max_fps: maximum_fps.max(1),
        adaptive: false,
    });
    let hooks = seams::frame_hooks(
        &context, &memory, bundle, &settings, &run, &screen, &refresh,
    )?;
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
        if let Err(error) = nxkit_svc_audio::set_active(active) {
            eprintln!("nxkit audio: {error}");
        }
        nxkit_rt::nvn::set_active(active);
        if active {
            s.link.set_paused(false);
        }
    });
}
