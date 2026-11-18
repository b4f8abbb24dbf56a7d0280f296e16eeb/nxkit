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

use crate::error::Result;
use crate::manifest::Game;
use crate::sys::appkit::{self, AppEvents, MouseEvent, MouseMode, WindowConfig};
use crate::{seams, startup, stop, sys, thread};

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

/// Runs the game. It returns when the window or the game has stopped.
pub fn run(opts: &Options) -> Result<()> {
    sys::crashsig::install();
    let game = Game::read(&opts.root)?;
    let mask = startup::service_mask(&game)?;
    let startup::Prepared {
        loaded,
        memory: gpu_memory,
        context: ctx,
        layout,
        boot,
    } = startup::prepare(&game, mask, &game.root)?;
    let settings = loaded.game.settings.clone();

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
