//! The startup order of the host (R47).
//!
//! 1. Read `game.json`.
//! 2. Place the modules: dyld maps the library, or the host maps the code from the library file and copies the data,
//!    and the one reservation holds everything, registered with `nxkit_abi::guest`. The GPU buffer over it comes
//!    next, and then the guard page.
//! 3. Start the kernel.
//! 4. Register the kernel and the IPC server and start the services.
//! 5. Finish the load: relocations, imports and gates.
//! 6. Open the window.
//! 7. Start the main guest thread, which boots the SDK and runs `nnMain` (R57).
//! 8. Run the AppKit loop until the game or the window stops.
//!
//! The patches of the title are not applied here. The build applied them to the images (R2).

use crate::hostenv::RunSettings;
use std::path::PathBuf;
use std::sync::Arc;

use nxkit_metal::Context;

use crate::boot::Boot;
use crate::error::{HostError, Result};
use crate::graphics::Graphics;
use crate::manifest::Game;
use crate::sys::appkit::{self, AppEvents, MouseEvent, MouseMode, WindowConfig};
use crate::{guestsym, loader, place, seams, stop, sys, thread};

/// What the command line asks for.
#[derive(Debug, Clone)]
pub struct Options {
    /// The run folder, which holds `game.json`.
    pub root: PathBuf,
    /// Start in full screen.
    pub fullscreen: bool,
}

struct Events;

impl AppEvents for Events {
    fn focus_changed(&self, focused: bool) {
        seams::focus_changed(focused);
    }

    fn closing(&self) {
        stop::request("the window closed");
    }

    fn mouse(&self, event: MouseEvent) {
        seams::mouse(event);
    }

    fn mouse_mode(&self) -> MouseMode {
        seams::mouse_mode()
    }

    fn home_buttons(&self) -> crate::home::Buttons {
        seams::home_buttons()
    }

    fn home_input(&self, blocked: bool) {
        nxkit_svc_input::set_blocked(blocked);
    }

    fn game_focus(&self, focus: crate::home::GameFocus) {
        seams::game_focus(focus);
    }

    fn quit(&self, why: &'static str) {
        stop::request(why);
    }
}

/// Starts the control socket of the run (W35). A failure leaves the game running without it.
fn start_control(board: nxkit_abi::live::Board, game: &crate::manifest::Game, run: &RunSettings) {
    let mut info = vec![
        ("pid".to_string(), std::process::id().to_string()),
        ("game".to_string(), game.name.clone()),
        ("title_id".to_string(), game.title_id.clone()),
        ("version".to_string(), game.version.clone()),
    ];
    #[cfg(target_os = "macos")]
    info.push((
        "diag_dir".to_string(),
        run.gpu.diag_dir.display().to_string(),
    ));
    #[cfg(not(target_os = "macos"))]
    let _ = run;
    // Not `mut` where no part adds a verb.
    #[allow(unused_mut)]
    let mut control = crate::control::Control::new(board, info);
    #[cfg(target_os = "macos")]
    crate::shaderctl::shader_verbs(&mut control, &run.shader_hub);
    match crate::control::start(control) {
        Ok(path) => eprintln!("nxkit-host: control socket {}", path.display()),
        Err(e) => eprintln!("nxkit-host: no control socket: {e}"),
    }
}

fn check_kernel_service(mask: u64) -> Result<()> {
    if mask & nxkit_rt::Service::Kernel.bit() == 0 {
        return Err(HostError::Manifest(
            "\"services\" must include \"kernel\", because every svc of the game goes through it"
                .into(),
        ));
    }
    Ok(())
}

/// Runs the game. It returns when the window or the game has stopped.
pub fn run(opts: &Options) -> Result<()> {
    // The one read of the `NXKIT_*` environment (W27): every crate's typed settings come from this snapshot. The
    // crates that keep them for code with no context get them now, before any part exists. A crate that had them
    // already or was read before stops the run. The rest is passed in below. The saved choices of the player in the
    // preferences of the app fill the launch options that the environment leaves unset.
    let mut snapshot = nxkit_abi::env::Env::capture();
    sys::saved_choices(&mut snapshot);
    let run = RunSettings::from_env(&snapshot);
    run.install()
        .map_err(|e| HostError::Settings(e.to_string()))?;
    // The live keys (W35) get their values of the start now too. The board changes them later, on a command of the
    // control socket.
    let board = nxkit_abi::live::Board::new(snapshot.clone(), crate::hostenv::live_sections());
    let env = &run.host;
    sys::hud::enable(env.metal_hud);
    sys::crashsig::install(env);
    // From here the log goes through a pipe, so a slow disk under stderr never holds a game thread (`logpipe`).
    sys::logpipe::install();
    let game = Game::read(&opts.root, env)?;
    sys::loading::set_title(&game.name);
    let mask = loader::service_mask(&game.services)?;
    check_kernel_service(mask)?;
    let placed = place::place(&game)?;
    let mem = placed.mem;
    let (layout, settings) = (mem.layout, placed.game.settings.clone());
    // What the screen showed of the presents: the backend writes it, the NVN present and the refresh use it. The
    // refresh plans the link for the screen: the pacing seam, the window and the tick hook share it (W26).
    let screen = Arc::new(nxkit_nvn::screen::Screen::new(Some(
        crate::frames::shown_to_step,
    )));
    let refresh = Arc::new(crate::refresh::Refresh::new(
        crate::refresh::Request::from_settings(&settings, env)
            .at_120(seams::runs_at_120(&placed.game)),
        screen.clone(),
    ));

    // The GPU buffer comes first, while no page of the reservation is `PROT_NONE` (Metal refuses such a range).
    let ctx = Context::system_default().map_err(|e| HostError::App(format!("Metal: {e}")))?;
    let gpu_memory = Arc::new(mem.gpu_memory(&ctx)?);
    mem.keep_gpu_pages(env);
    mem.seal()?;
    crate::memreport::start(&ctx, &layout, env);

    nxkit_rt::nxkit_init(mask);
    let arena = crate::obswire::arena_size(&placed.game, &run.ue, layout.alias.len)?;
    sys::kernel_init(thread::guest_thread_entry, &layout, &placed.game, arena)?;
    seams::register_kernel()?;
    seams::register_server(mask)?;
    seams::init_services(&placed.game, mask, env, &refresh)?;

    let program_args = placed.game.program_args(placed.game.settings.render_dims());
    let mut loaded = loader::load(placed, mask)?;
    crate::stall::start(env.stall_dump);
    eprintln!(
        "nxkit-host: {} modules loaded ({:?}), {} exports",
        loaded.game.modules.len(),
        loaded.placement,
        loaded.exports.len()
    );
    sys::crashsig::set_modules(
        loaded
            .game
            .modules
            .iter()
            .map(|m| {
                let start = m.base.wrapping_add_signed(m.low);
                (m.name.clone(), start, start + m.span)
            })
            .collect(),
    );
    nxkit_metal::set_pc_namer(sys::crashsig::guest_place);
    guestsym::set(std::mem::take(&mut loaded.exports));
    let boot = Boot::resolve(std::mem::take(&mut loaded.inits), &program_args)?;

    // Only the layer of the game's graphics API is made (R64).
    let hooks = match loaded.game.graphics {
        Graphics::Nvn => seams::frame_hooks(
            &ctx,
            &gpu_memory,
            &opts.root,
            &settings,
            &run,
            &screen,
            &refresh,
        )?,
        Graphics::Vulkan => seams::vulkan_frame_hooks(&ctx, &opts.root, &settings, &run, &refresh)?,
    };
    // Over a launcher the window shows with the first picture that is not black. An NVN game's presents tell it.
    let probed = env.overlay && matches!(loaded.game.graphics, Graphics::Nvn);
    if probed {
        nxkit_gpu::when_shown(crate::sys::present_picture_done);
    }
    let window = WindowConfig {
        title: format!("nxkit {} {}", loaded.game.title_id, loaded.game.version),
        size: None,
        fullscreen: opts.fullscreen || env.fullscreen,
        metal_hud: env.metal_hud,
        home: crate::home::Setup::new(
            env.home_menu,
            env.home_quit,
            env.launcher.clone(),
            env.launcher_pid,
            env.home_text.as_deref(),
            &loaded.game.name,
        ),
        probed,
    };
    if env.control {
        start_control(board, &loaded.game, &run);
    }
    // The Home button reaches the app only on a controller that GameController gives it to.
    nxkit_svc_input::claim_home(window.home.is_some());
    let stack_top = layout.main_stack.end();
    let result = appkit::run(
        &window,
        &ctx,
        hooks,
        refresh,
        Arc::new(Events),
        stop::flag(),
        move || thread::start_main(boot, stack_top),
    );
    drop(loaded);
    eprintln!(
        "nxkit-host: region use {}",
        crate::memreport::region_use(&layout)
    );
    eprintln!("{}", crate::memreport::end_line(&layout));
    for l in crate::texreport::lines() {
        eprintln!("{l}");
    }
    result
}
