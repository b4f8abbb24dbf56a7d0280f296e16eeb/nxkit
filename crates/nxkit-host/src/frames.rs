//! The frame path of the host: the GPU backend, the pacer and the hooks the display link calls.
//!
//! `frame_hooks` in `seams.rs` calls [`install`]. The backend of `nxkit-gpu` answers the NVN front end of `nxkit-rt`.
//! It encodes each flushed frame and queues each presented frame. The [`Pacer`] of `nxkit-metal` picks, on each tick of
//! the display link, which queued frame to show, and the backend blits it into the drawable.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use nxkit_abi::platform::{self, Platform};
use nxkit_gpu::{GpuBackend, GpuSettings};
use nxkit_metal::{
    Context, DisplayTick, FrameHooks, FrameInterval, FrameSource, GobOrder, GuestMemory, Pacer, PolicyInput,
    choose_present_plan,
};
use nxkit_nvn::backend::Backend;
use nxkit_rt::nvn::{self, CommandSettings};

use crate::error::{HostError, Result};
use crate::settings::Settings;

/// The rate of the display link the plan is made for.
const DISPLAY: FrameInterval = FrameInterval::Hz120;

/// The rate of the game's own loop. A Switch game steps at 60.
const GAME_HZ: u32 = 60;

/// The file of the Metal 4 pipeline archive in the run folder.
const ARCHIVE: &str = "pipelines.mtl4archive";

/// The folder of the translated shaders in the run folder.
const SHADERS: &str = "shaders";

/// The settings of the GPU backend, from the keys of `game.json` that the encoder uses. `enc_thread` moves the encode
/// of each frame to a thread of its own. A game of the Switch 2 gets its order inside a GOB.
pub fn gpu_settings(root: &Path, settings: &Settings) -> GpuSettings {
    GpuSettings {
        shader_dir: root.join(SHADERS),
        archive: settings.pso_mtl4.then(|| root.join(ARCHIVE)),
        // Zero picks the number of encode threads from the cores. `NXKIT_ENCODE_THREADS` sets it by hand.
        threads: std::env::var("NXKIT_ENCODE_THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or(0),
        // Scaling is off until every attachment of a pass gets the same scale decision.
        scale: std::env::var("NXKIT_SCALE").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0),
        enc_thread: settings.enc_thread,
        gob_order: match platform::get() {
            Platform::Nx => GobOrder::Tegra,
            Platform::Ounce => GobOrder::Switch2,
        },
        ..GpuSettings::default()
    }
}

/// The settings of the NVN front end, from the keys of `game.json`. A game of the Switch 2 gets the API version of
/// NVN2: 231 and the minor that the driver in its `sdk` answers, 39 when the `sdk` gives none. Games check it and make
/// no graphics system on a mismatch. `nvn_api` of `game.json` wins over both.
fn nvn_settings(settings: &Settings) -> nvn::Settings {
    let mut s = nvn::Settings::default();
    if platform::get() == Platform::Ounce {
        s.api_major = nvn::NVN2_API_MAJOR;
        s.api_minor = nvn::sdk_item(1).unwrap_or(nvn::NVN2_API_MINOR);
        eprintln!("nxkit-host: NVN2 device API {}.{} (Switch 2)", s.api_major, s.api_minor);
    }
    if let Some((major, minor)) = settings.nvn_api {
        (s.api_major, s.api_minor) = (major, minor);
    }
    let (offset, length) = settings.shader_key_skip;
    if length > 0 {
        s.shader_key_skip = u32::try_from(offset).ok().zip(u32::try_from(length).ok());
    }
    s
}

/// The hooks of the display link. They step the input state, tell the pacing service about the tick, follow the hold that it asks for, and
/// pass the tick to the pacer.
struct Ticks {
    pacer: Arc<Pacer>,
    hold: AtomicU32,
}

impl FrameHooks for Ticks {
    fn display_tick(&self, tick: DisplayTick) {
        nxkit_svc_input::tick();
        // `Err` means the pacing service is not initialised. The pacer then keeps the source of the plan.
        let _ = nxkit_svc_pacing::on_display_tick();
        let hold = nxkit_svc_pacing::hold().ok().and_then(|h| u32::try_from(h).ok()).unwrap_or(0);
        if hold > 0 && self.hold.swap(hold, Ordering::Relaxed) != hold {
            let per_vsync = nxkit_svc_pacing::snapshot().map_or(1, |s| s.ticks_per_vsync.max(1));
            self.pacer.set_source(FrameSource::Hold { ticks_per_frame: hold * per_vsync });
        }
        self.pacer.display_tick(tick);
    }
}

/// Makes the backend, installs it in the NVN front end and returns the hooks for the display link.
pub fn install(ctx: &Context, memory: &Arc<GuestMemory>, root: &Path, settings: &Settings) -> Result<Arc<dyn FrameHooks>> {
    let backend = GpuBackend::new(ctx, memory.clone(), &gpu_settings(root, settings))
        .map_err(|e| HostError::App(format!("the GPU backend: {e}")))?;
    nvn::install(backend.clone() as Arc<dyn Backend>, nvn_settings(settings))
        .map_err(|e| HostError::App(format!("the NVN front end: {e:?}")))?;
    if !nvn::set_command_settings(CommandSettings { ignore_null_stream_binds: settings.vertex_stream_null_bind }) {
        return Err(HostError::App("the NVN command settings came after the first command buffer".into()));
    }
    let plan = choose_present_plan(&PolicyInput {
        title: None,
        frame_step: false,
        game_hz: GAME_HZ,
        display: DISPLAY,
        interpolation: false,
    });
    eprintln!("nxkit-host: present plan {:?} ({})", plan.source, plan.reason);
    let pacer = Pacer::new(plan, backend.clone());
    backend.set_pacer(pacer.clone());
    Ok(Arc::new(Ticks { pacer, hold: AtomicU32::new(0) }))
}
