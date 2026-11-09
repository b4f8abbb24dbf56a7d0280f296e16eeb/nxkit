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

use std::path::PathBuf;
use std::sync::Arc;

use nxkit_metal::Context;

use crate::boot::Boot;
use crate::error::{HostError, Result};
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
    let game = Game::read(&opts.root)?;
    let mask = loader::service_mask(&game.services)?;
    check_kernel_service(mask)?;
    let placed = place::place(&game)?;
    let mem = placed.mem;
    let (layout, settings) = (mem.layout, placed.game.settings.clone());

    // The GPU buffer comes first, while no page of the reservation is `PROT_NONE` (Metal refuses such a range).
    let ctx = Context::system_default().map_err(|e| HostError::App(format!("Metal: {e}")))?;
    let gpu_memory = Arc::new(mem.gpu_memory(&ctx)?);
    mem.seal()?;

    nxkit_rt::nxkit_init(mask);
    sys::kernel_init(thread::guest_thread_entry, &layout, &placed.game)?;
    seams::register_kernel()?;
    seams::register_server(mask)?;
    seams::init_services(&placed.game, mask)?;

    let mut loaded = loader::load(placed, mask)?;
    eprintln!(
        "nxkit-host: {} modules loaded ({:?}), {} exports",
        loaded.game.modules.len(),
        loaded.placement,
        loaded.exports.len()
    );
    guestsym::set(std::mem::take(&mut loaded.exports));
    let boot = Boot::resolve(std::mem::take(&mut loaded.inits))?;

    crate::refresh::configure(crate::refresh::Request::from_settings(&settings));
    let hooks = seams::frame_hooks(&ctx, &gpu_memory, &opts.root, &settings)?;
    let window = WindowConfig {
        title: format!("nxkit {} {}", loaded.game.title_id, loaded.game.version),
        size: None,
        fullscreen: opts.fullscreen,
    };
    let stack_top = layout.main_stack.end();
    let result = appkit::run(
        &window,
        &ctx,
        hooks,
        Arc::new(Events),
        stop::flag(),
        move || thread::start_main(boot, stack_top),
    );
    drop(loaded);
    result
}
