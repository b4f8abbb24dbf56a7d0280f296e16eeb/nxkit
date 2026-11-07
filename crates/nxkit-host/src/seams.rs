//! The seams: every place where the host needs an API that another crate builds.
//!
//! A seam is a function in this file. The integration step replaces its body. A seam that cannot work yet returns
//! `HostError::Unwired` with the name of what is missing, and the host stops at start with that message (R15).
//! `docs/nxkit-host.md` lists the seams and what each one needs.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use nxkit_metal::{Context, DisplayTick, FrameHooks, GuestMemory};

use crate::error::{HostError, Result};
use crate::layout::Layout;
use crate::manifest::{Game, MainThread, Memory};
use crate::settings::Settings;

/// Registers the kernel with `nxkit_abi::kernel::set_kernel`, so that the services can ask for the current thread, wait
/// and signal.
///
/// Needs: `impl nxkit_abi::kernel::Kernel for nxkit_hos::Kernel`, and an accessor in `nxkit-rt` that returns the
/// `Arc<Kernel>` that `nxkit_kernel_init` made.
pub fn register_kernel() -> Result<()> {
    Err(HostError::Unwired(
        "nxkit_abi::kernel::set_kernel (the abi Kernel for nxkit_hos::Kernel, and an accessor for the kernel in nxkit-rt)",
    ))
}

/// Gives the kernel the regions of the reservation and the memory profile of the game. The heap is for
/// `SetHeapSize`, the alias region for `MapMemory`, and the stack region is the destination of `MapMemory` for
/// stacks. `memory` has the application pool, the used memory, the system resource size and the map limit that
/// `GetInfo` and the memory svcs use. `settings.core_preempt` says whether the scheduler preempts threads of one core.
///
/// Needs: a kernel call that takes the ranges of `layout`, the numbers of `memory` and `core_preempt`.
pub fn configure_kernel(layout: &Layout, memory: &Memory, settings: &Settings) -> Result<()> {
    let _ = (layout, memory, settings);
    Err(HostError::Unwired(
        "the kernel's memory regions and profile (a kernel call that takes the heap, alias and stack ranges, the application pool, the used memory, the system resource size, the map limit and core_preempt)",
    ))
}

/// Makes the calling host thread the main thread of the guest, with the priority, the core and the stack size of
/// the NPDM.
///
/// Needs: a kernel call that attaches the current thread with a priority and a core. `nxkit_svc` attaches an unknown
/// thread with priority 44 and core -2, which is not the NPDM.
pub fn attach_main_thread(spec: &MainThread) -> Result<()> {
    let _ = spec;
    Err(HostError::Unwired(
        "attaching the main guest thread with the NPDM priority and core (nxkit-rt or nxkit-hos)",
    ))
}

/// Builds the IPC server from the `services()` of every service crate and registers it with
/// `nxkit_abi::kernel::set_server`, so that `SendSyncRequest` reaches it.
///
/// Needs: the function of `nxkit-ipc` that builds a server from all `services()`.
pub fn register_server(game: &Game) -> Result<()> {
    let _ = game;
    Err(HostError::Unwired(
        "the IPC server (nxkit-ipc: build a server from all services() and set_server)",
    ))
}

/// The names of the exports that service crates ask to run on a big stack, from `pub fn big_stack_exports()` of each
/// service crate. The loader puts a big-stack stub (`bigstack.rs`) into the import slots of these.
///
/// Needs: `big_stack_exports()` in the service crates, and the collection of them in `nxkit-rt` next to `entries()`.
/// Without it no export runs on a big stack, so every service function runs on the stack of the guest thread.
pub fn big_stack_exports() -> HashSet<String> {
    HashSet::new()
}

/// Starts the service crates with the configuration of the game. The services are the names in `game.services`.
///
/// The fs service gets `game.aoc_dirs()` for its `Config.aoc` and the romfs folder. The applet service gets
/// `settings.launch_param`, `args`, `env`, `op_mode`, `perf_boost` and `display_versions`. The input service gets
/// `pad_layout`. The video and pacing services get `fps_*`, `present_*` and `screen_aspect`. The gpu service gets the
/// `tex_*`, `render_cond`, `vconv_range`, `gpu_ts_segment`, `draw_barriers_game`, `shader_key_skip`, `upscaler_keys`,
/// `pso_mtl4`, `enc_thread` and `nvn_api` keys. The keys in `settings.extra` go unread to the service that the title
/// module addressed, through its `nxtitle` runtime hook.
///
/// No service crate has an `init` at this commit, so there is nothing to call. The integration calls the `init`
/// of each service here.
pub fn init_services(game: &Game) -> Result<()> {
    let _ = game;
    Ok(())
}

/// The window gained or lost the key focus. The applet service reports it to the game as a focus change, and the
/// input service releases held keys on a loss.
///
/// Needs: the `system` and `input` services.
pub fn focus_changed(focused: bool) {
    let _ = focused;
}

/// Counts the display ticks and drops their drawables. It is the stand-in until the encoder is connected.
#[derive(Debug, Default)]
struct DropFrames {
    ticks: AtomicU64,
}

impl FrameHooks for DropFrames {
    fn display_tick(&self, tick: DisplayTick) {
        if self.ticks.fetch_add(1, Ordering::Relaxed) == 0 {
            eprintln!("nxkit-host: no encoder is connected, the frames of the display are dropped (see seams.rs)");
        }
        drop(tick);
    }
}

/// The hooks that the display link calls. They connect the encoder of `nxkit-gpu` and the pacing of `nxkit-metal`
/// to the window: on each display tick they present the frame that the guest finished. `settings` has the pacing and
/// encoder keys (`fps_target`, `fps_max`, `fps_hold`, `present_snap`, `pso_mtl4`, `enc_thread` and the `tex_*` keys).
///
/// Needs: the encoder (`nxkit-gpu`) over `memory`, which is the reservation as the GPU buffer, and the pacing
/// service.
pub fn frame_hooks(ctx: &Context, memory: &Arc<GuestMemory>, settings: &Settings) -> Result<Arc<dyn FrameHooks>> {
    let _ = (ctx, memory, settings);
    Ok(Arc::new(DropFrames::default()))
}
