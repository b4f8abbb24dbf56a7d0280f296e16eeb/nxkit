//! The startup order of the host (R47).
//!
//! 1. Read `game.json`. 2. Reserve the guest memory and register it with `nxkit_abi::guest`. 3. Start the kernel.
//! 4. Register the kernel and the IPC server and start the services. 5. Load the modules, fill the imports and
//! install the gates. 6. Open the window. 7. Start the main guest thread. 8. Run the AppKit loop until the game or
//! the window stops.
//!
//! The patches of the title are not applied here. The build applied them to the images (R2).

use std::path::PathBuf;
use std::sync::Arc;

use nxkit_metal::Context;

use crate::error::{HostError, Result};
use crate::sys::appkit::{self, AppEvents, WindowConfig};
use crate::manifest::Game;
use crate::{layout, loader, memory, seams, stop, sys, thread};

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
}

fn check_kernel_service(mask: u64) -> Result<()> {
    if mask & nxkit_rt::Service::Kernel.bit() == 0 {
        return Err(HostError::Manifest(
            "\"services\" must include \"kernel\", because every svc of the game goes through it".into(),
        ));
    }
    Ok(())
}

/// Runs the game. It returns when the window or the game has stopped.
pub fn run(opts: &Options) -> Result<()> {
    let game = Game::read(&opts.root)?;
    let mask = loader::service_mask(&game.services)?;
    check_kernel_service(mask)?;
    let mem = memory::init(layout::plan(&game)?)?;

    nxkit_rt::nxkit_init(mask);
    sys::kernel_init(thread::guest_thread_entry)?;
    seams::register_kernel()?;
    seams::configure_kernel(&mem.layout, &game.memory)?;
    seams::register_server(&game)?;
    seams::init_services(&game)?;

    let loaded = loader::load(&game, mem, mask)?;
    eprintln!(
        "nxkit-host: {} modules loaded ({:?}), entry {:#x}, {} exports",
        game.modules.len(),
        loaded.placement,
        loaded.entry,
        loaded.exports.len()
    );

    let ctx = Context::system_default().map_err(|e| HostError::App(format!("Metal: {e}")))?;
    let gpu_memory = Arc::new(mem.res.as_guest_memory(&ctx)?);
    let hooks = seams::frame_hooks(&ctx, &gpu_memory)?;
    let window = WindowConfig {
        title: format!("nxkit {} {}", game.title_id, game.version),
        width: 1280.0,
        height: 720.0,
        fullscreen: opts.fullscreen,
    };
    let (entry, stack_top, spec) = (loaded.entry, mem.layout.main_stack.end(), game.main_thread);
    appkit::run(&window, &ctx, hooks, Arc::new(Events), stop::flag(), move || {
        thread::start_main(entry, stack_top, spec)
    })
}
