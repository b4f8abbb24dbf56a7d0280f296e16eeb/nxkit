//! The startup order of the host (R47).
//!
//! 1. Read `game.json`. 2. Place the modules: dyld maps the library, or the host copies the segments, and the one
//! reservation holds everything, registered with `nxkit_abi::guest`. 3. Start the kernel. 4. Register the kernel and
//! the IPC server and start the services. 5. Finish the load: relocations, imports and gates. 6. Open the window.
//! 7. Start the main guest thread. 8. Run the AppKit loop until the game or the window stops.
//!
//! The patches of the title are not applied here. The build applied them to the images (R2).

use std::path::PathBuf;
use std::sync::Arc;

use nxkit_metal::Context;

use crate::error::{HostError, Result};
use crate::sys::appkit::{self, AppEvents, WindowConfig};
use crate::manifest::Game;
use crate::{loader, place, seams, stop, sys, thread};

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
    let placed = place::place(&game)?;
    let mem = placed.mem;
    let (layout, memory, settings) = (mem.layout, placed.game.memory, placed.game.settings.clone());

    nxkit_rt::nxkit_init(mask);
    sys::kernel_init(thread::guest_thread_entry)?;
    seams::register_kernel()?;
    seams::configure_kernel(&layout, &memory, &settings)?;
    seams::register_server(&placed.game)?;
    seams::init_services(&placed.game)?;

    let loaded = loader::load(placed, mask)?;
    eprintln!(
        "nxkit-host: {} modules loaded ({:?}), entry {:#x}, {} exports",
        loaded.game.modules.len(),
        loaded.placement,
        loaded.entry,
        loaded.exports.len()
    );

    let ctx = Context::system_default().map_err(|e| HostError::App(format!("Metal: {e}")))?;
    let gpu_memory = Arc::new(mem.res.as_guest_memory(&ctx)?);
    let hooks = seams::frame_hooks(&ctx, &gpu_memory, &settings)?;
    let window = WindowConfig {
        title: format!("nxkit {} {}", loaded.game.title_id, loaded.game.version),
        size: None,
        fullscreen: opts.fullscreen,
    };
    let (entry, stack_top, spec) = (loaded.entry, layout.main_stack.end(), loaded.game.main_thread);
    let result = appkit::run(&window, &ctx, hooks, Arc::new(Events), stop::flag(), move || {
        thread::start_main(entry, stack_top, spec)
    });
    drop(loaded);
    result
}
