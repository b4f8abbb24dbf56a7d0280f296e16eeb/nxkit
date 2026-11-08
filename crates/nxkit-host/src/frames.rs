//! The frame path of the host: the GPU backend, the pacer and the hooks the display link calls.
//!
//! `frame_hooks` in `seams.rs` calls [`install`]. The backend of `nxkit-gpu` answers the NVN front end of `nxkit-rt`.
//! It encodes each flushed frame and queues each presented frame. The [`Pacer`] of `nxkit-metal` picks, on each tick of
//! the display link, which queued frame to show, and the backend blits it into the drawable.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use nxkit_abi::platform::{self, Platform};
use nxkit_gpu::{GpuBackend, GpuSettings};
use nxkit_metal::{
    Context, DisplayTick, FrameHooks, FrameInterval, FrameSource, GobOrder, GuestMemory, Pacer,
    PolicyInput, choose_present_plan,
};
use nxkit_nvn::backend::Backend;
use nxkit_rt::nvn::{self, CommandSettings};
use nxkit_svc_pacing::refresh::RateTracker;

use crate::error::{HostError, Result};
use crate::settings::Settings;

/// The rate of the display link the first plan is made for. The window replaces it with the real one at its first tick.
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
        // The archive is loaded whenever the build made one: `pso_mtl4` came from the engine's profiles, and a title
        // without it (Ocarina of Time) left its 4.8 GB archive unused. The setting still turns it on for an app whose
        // archive sits elsewhere.
        archive: {
            let path = root.join(ARCHIVE);
            (settings.pso_mtl4 || path.is_file()).then_some(path)
        },
        // Zero picks the number of encode threads from the cores. `NXKIT_ENCODE_THREADS` sets it by hand.
        threads: std::env::var("NXKIT_ENCODE_THREADS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        // Scaling is off until every attachment of a pass gets the same scale decision.
        scale: std::env::var("NXKIT_SCALE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1.0),
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
        eprintln!(
            "nxkit-host: NVN2 device API {}.{} (Switch 2)",
            s.api_major, s.api_minor
        );
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
/// pass the tick to the pacer. They also measure the real tick rate from the target times of the ticks.
struct Ticks {
    pacer: Arc<Pacer>,
    /// The hold and the ticks per vsync that the pacer's source was made from.
    applied: Mutex<(u32, u32)>,
    rate: Mutex<RateTracker>,
    ticks: AtomicU64,
    /// The generation of the plan the tracker measures.
    seen: AtomicU64,
}

impl FrameHooks for Ticks {
    fn display_tick(&self, tick: DisplayTick) {
        nxkit_svc_input::tick();
        // `Err` means the pacing service is not initialised. The pacer then keeps the source of the plan.
        let _ = nxkit_svc_pacing::on_display_tick();
        let hold = nxkit_svc_pacing::hold()
            .ok()
            .and_then(|h| u32::try_from(h).ok())
            .unwrap_or(0);
        let per_vsync = nxkit_svc_pacing::snapshot().map_or(1, |s| s.ticks_per_vsync.max(1));
        // A change of the hold, or of the ticks per vsync (the screen changed its rate), makes a new source. Without a
        // hold from the frame step, a frame stays one vsync.
        let key = (hold, per_vsync);
        {
            let mut applied = self.applied.lock().unwrap_or_else(PoisonError::into_inner);
            if *applied != key {
                *applied = key;
                self.pacer.set_source(FrameSource::Hold {
                    ticks_per_frame: hold.max(1) * per_vsync,
                });
            }
        }
        let n = self.ticks.fetch_add(1, Ordering::Relaxed) + 1;
        let mut rate = self.rate.lock().unwrap_or_else(PoisonError::into_inner);
        let generation = crate::refresh::generation();
        if self.seen.swap(generation, Ordering::Relaxed) != generation {
            rate.reset();
        }
        rate.tick(tick.target_presentation_timestamp);
        if n.is_multiple_of(60)
            && let Some(hz) = rate.hz()
            && crate::refresh::note_measured(hz).is_some()
        {
            rate.reset();
        }
        drop(rate);
        self.pacer.display_tick(tick);
    }
}

/// Makes the backend, installs it in the NVN front end and returns the hooks for the display link.
pub fn install(
    ctx: &Context,
    memory: &Arc<GuestMemory>,
    root: &Path,
    settings: &Settings,
) -> Result<Arc<dyn FrameHooks>> {
    let backend = GpuBackend::new(ctx, memory.clone(), &gpu_settings(root, settings))
        .map_err(|e| HostError::App(format!("the GPU backend: {e}")))?;
    nvn::install(backend.clone() as Arc<dyn Backend>, nvn_settings(settings))
        .map_err(|e| HostError::App(format!("the NVN front end: {e:?}")))?;
    if !nvn::set_command_settings(CommandSettings {
        ignore_null_stream_binds: settings.vertex_stream_null_bind,
    }) {
        return Err(HostError::App(
            "the NVN command settings came after the first command buffer".into(),
        ));
    }
    let plan = choose_present_plan(&PolicyInput {
        title: None,
        frame_step: false,
        game_hz: GAME_HZ,
        display: DISPLAY,
        interpolation: false,
    });
    eprintln!(
        "nxkit-host: present plan {:?} ({})",
        plan.source, plan.reason
    );
    let pacer = Pacer::new(plan, backend.clone());
    backend.set_pacer(pacer.clone());
    Ok(Arc::new(Ticks {
        pacer,
        applied: Mutex::new((0, 0)),
        rate: Mutex::new(RateTracker::new()),
        ticks: AtomicU64::new(0),
        seen: AtomicU64::new(0),
    }))
}
