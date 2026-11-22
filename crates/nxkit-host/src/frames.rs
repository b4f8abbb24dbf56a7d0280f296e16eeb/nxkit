//! The frame path of the host: the GPU backend, the pacer and the hooks the display link calls.
//!
//! `frame_hooks` in `seams.rs` calls [`install`]. The backend of `nxkit-gpu` answers the NVN front end of `nxkit-rt`.
//! It encodes each flushed frame and queues each presented frame. The [`Pacer`] of `nxkit-metal` picks, on each tick of
//! the display link, which queued frame to show, and the backend blits it into the drawable.
//!
//! A game on Vulkan gets [`install_vulkan`] instead: the backend of `nxkit-vk-gpu` answers the Vulkan front end of
//! `nxkit-rt`, and the same pacer and display hooks show its swapchain images. Only the layer of the game's API is made.

use crate::hostenv::RunSettings;
use nxkit_nvn::screen::Screen;
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

/// The folder of the pipeline archive shards in the run folder. `nxkit_abi::shards::SHARD_DIR` names it.
const SHARDS: &str = nxkit_abi::shards::SHARD_DIR;

/// The folder of the translated shaders in the run folder.
const SHADERS: &str = "shaders";

/// The settings of the GPU backend, from the keys of `game.json` that the encoder uses. `enc_thread` moves the encode
/// of each frame to a thread of its own. A game of the Switch 2 gets its order inside a GOB.
pub fn gpu_settings(
    root: &Path,
    settings: &Settings,
    run: &RunSettings,
    screen: &Arc<Screen>,
) -> GpuSettings {
    GpuSettings {
        shader_dir: root.join(SHADERS),
        // The archive is loaded whenever the build made one: `pso_mtl4` came from the engine's profiles, and a title
        // without it (Ocarina of Time) left its 4.8 GB archive unused. The setting still turns it on for an app whose
        // archive sits elsewhere.
        archive: {
            let path = root.join(ARCHIVE);
            (settings.pso_mtl4 || path.is_file()).then_some(path)
        },
        // The shards of a build that made them (an index in the folder): the archive is not read whole, a shard is opened
        // for the first pipeline in it. `NXKIT_ARCHIVE_SHARDS=0` ignores them and loads the single archive, if any.
        archive_shards: {
            let dir = root.join(SHARDS);
            let on = run.host.archive_shards_off;
            (!on && dir.join(nxkit_abi::shards::INDEX_FILE).is_file()).then_some(dir)
        },
        // `NXKIT_SHARD_LRU` sets how many shards stay open. Zero is 32.
        open_shards: run.host.shard_lru,
        // Zero picks the number of encode threads from the cores. `NXKIT_ENCODE_THREADS` sets it by hand.
        threads: run.host.encode_threads,
        // `NXKIT_SCALE` is a diagnostic: a pass can mix scaled and native attachments (R70).
        scale: run.host.scale,
        // `NXKIT_PRESENT_UPSCALE`, a launch option: MetalFX in the present (R70).
        present_upscale: run.host.present_upscale,
        // `temporal_upscale` of the title: MetalFX temporal upscaling in place of the game's upscale pass (R72).
        // `NXKIT_TEMPORAL_UPSCALE=0` turns it off in the encoder.
        temporal_upscale: settings.temporal_upscale.as_ref().map(temporal_upscale),
        compile_bases: run.gpu_bases,
        specialize: run.gpu_specialize,
        // The encoder makes the pipelines of the recorded list before it returns; a long wait shows a loading screen.
        prewarm_screen: nxkit_gpu::ScreenHook(Some(Arc::new(crate::sys::loading::LoadingScreen))),
        // The `frame` and `gateseq` traces of `nxkit-rt` time the batches of the encoder.
        batch_hooks: nxkit_rt::nvn::batch_hooks(),
        screen: screen.clone(),
        enc_thread: settings.enc_thread,
        overlap: settings.overlap,
        // The runtime pipeline cache: what this run compiles is kept for the next start (`crate::pipecache`).
        runtime_cache: crate::pipecache::folder(root, &run.host),
        build_id: crate::pipecache::build_id(root),
        // The pipelines that the build resolved from the title's seeds: made at every start (`nxkit_pipestate::list`).
        shipped_list: Some(root.join(nxkit_pipestate::list::SHIPPED_LIST)).filter(|p| p.is_file()),
        gob_order: match platform::get() {
            Platform::Nx => GobOrder::Tegra,
            Platform::Ounce => GobOrder::Switch2,
        },
        ..GpuSettings::default()
    }
}

/// The temporal upscale of `game.json` in the types of the encoder.
fn temporal_upscale(t: &crate::tupscale::TemporalUpscale) -> nxkit_gpu::TemporalUpscale {
    let draw = |d: &crate::tupscale::DrawTexture| nxkit_gpu::DrawTexture {
        fragment: d.fragment.clone(),
        slot: d.slot,
    };
    nxkit_gpu::TemporalUpscale {
        upscale: draw(&t.upscale),
        source: nxkit_gpu::TemporalSource {
            fragment: t.source.fragment.clone(),
            depth_slot: t.source.depth_slot,
            velocity_slot: t.source.velocity_slot,
            view: nxkit_gpu::UboSlot {
                stage: match t.source.view.0 {
                    crate::tupscale::UboStage::Vertex => nxkit_gpu::detail::Stage::Vertex,
                    crate::tupscale::UboStage::Fragment => nxkit_gpu::detail::Stage::Fragment,
                },
                slot: t.source.view.1,
            },
        },
        passthrough: t.passthrough.iter().map(draw).collect(),
        view: nxkit_gpu::ViewLayout {
            clip_to_prev_clip: t.view[0],
            jitter: t.view[1],
            view_size: t.view[2],
        },
        velocity: nxkit_gpu::VelocityCode {
            scale: t.velocity_scale,
            bias: t.velocity_bias,
            y_up: t.velocity_y_up,
            zero_means_camera: t.zero_means_camera,
        },
        depth_reversed: t.depth_reversed,
    }
}

/// The settings of the NVN front end, from the keys of `game.json`. A game of the Switch 2 gets the API version of
/// NVN2: 231 and the minor that the driver in its `sdk` answers, 39 when the `sdk` gives none. Games check it and make
/// no graphics system on a mismatch. `nvn_api` of `game.json` wins over both.
fn nvn_settings(settings: &Settings, run: &RunSettings, screen: &Arc<Screen>) -> nvn::Settings {
    let mut s = nvn::Settings {
        env: run.rt,
        screen: screen.clone(),
        ..nvn::Settings::default()
    };
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
    s.commands = CommandSettings {
        ignore_null_stream_binds: settings.vertex_stream_null_bind,
    };
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
    /// The refresh plan: whether frames show when ready, and the rate the link should tick at.
    refresh: Arc<crate::refresh::Refresh>,
    /// The hold and the ticks per vsync that the pacer's source was made from.
    applied: Mutex<(u32, u32)>,
    rate: Mutex<RateTracker>,
    ticks: AtomicU64,
    /// The generation of the plan the tracker measures.
    seen: AtomicU64,
}

impl FrameHooks for Ticks {
    fn display_tick(&self, tick: DisplayTick) {
        // Variable refresh (`refresh.rs`): the pacer shows a frame as soon as it is ready, on the drawable of the newest
        // tick, and ignores the hold below.
        self.pacer.set_immediate(self.refresh.vrr_range().is_some());
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
        let generation = self.refresh.generation();
        if self.seen.swap(generation, Ordering::Relaxed) != generation {
            rate.reset();
        }
        rate.tick(tick.target_presentation_timestamp);
        if n.is_multiple_of(60)
            && let Some(hz) = rate.hz()
            && self.refresh.note_measured(hz).is_some()
        {
            rate.reset();
        }
        drop(rate);
        self.pacer.display_tick(tick);
    }
}

/// A present was shown at `secs` (`CACurrentMediaTime`). The variable frame step takes the time of the display from it,
/// on the clock of the starts of frames. It does nothing unless variable refresh turned it on.
pub(crate) fn shown_to_step(_present: u64, secs: f64) {
    shown_on(nxkit_rt::nvn::clock_ns(), secs);
}

/// [`shown_to_step`] on the clock of the Vulkan layer.
fn shown_to_step_vk(_present: u64, secs: f64) {
    shown_on(nxkit_rt::vk::clock_ns(), secs);
}

fn shown_on(now_ns: u64, secs: f64) {
    crate::stall::shown(now_ns);
    let ago_ns = ((nxkit_metal::media_time() - secs).max(0.0) * 1e9) as u64;
    nxkit_svc_pacing::frame_shown(now_ns.saturating_sub(ago_ns));
}

/// Makes the backend, installs it in the NVN front end and returns the hooks for the display link.
pub fn install(
    ctx: &Context,
    memory: &Arc<GuestMemory>,
    root: &Path,
    settings: &Settings,
    run: &RunSettings,
    screen: &Arc<Screen>,
    refresh: &Arc<crate::refresh::Refresh>,
) -> Result<Arc<dyn FrameHooks>> {
    crate::sys::exitflush::install();
    let backend = GpuBackend::new(
        ctx,
        memory.clone(),
        &nxkit_gpu::GpuSettings {
            timed_present: refresh.timed_present(),
            ..gpu_settings(root, settings, run, screen)
        },
    );
    let backend = backend.map_err(|e| HostError::App(format!("the GPU backend: {e}")))?;
    nvn::install(
        backend.clone() as Arc<dyn Backend>,
        nvn_settings(settings, run, screen),
    )
    .map_err(|e| HostError::App(format!("the NVN front end: {e:?}")))?;
    let pacer = Pacer::new(present_plan(), backend.clone());
    backend.set_pacer(pacer.clone());
    Ok(ticks(pacer, refresh))
}

/// The surface the Vulkan game sees: 1920x1080 docked, 1280x720 handheld (`op_mode`).
fn surface_extent(settings: &Settings) -> (u32, u32) {
    if settings.op_mode == "handheld" {
        (1280, 720)
    } else {
        (1920, 1080)
    }
}

/// Makes the Vulkan backend, installs it in the Vulkan front end and returns the hooks for the display link. Runs on the
/// loader thread before the game threads start, as the big-stack stubs need.
pub fn install_vulkan(
    ctx: &Context,
    root: &Path,
    settings: &Settings,
    refresh: &Arc<crate::refresh::Refresh>,
    present_upscale: nxkit_metal::PresentUpscale,
) -> Result<Arc<dyn FrameHooks>> {
    crate::sys::exitflush::install();
    let backend = nxkit_vk_gpu::VkGpu::new(
        ctx,
        nxkit_vk_gpu::VkGpuSettings {
            shader_dir: root.join(SHADERS),
            surface_extent: surface_extent(settings),
            timed_present: Some(refresh.timed_present()),
            on_shown: Some(Arc::new(shown_to_step_vk)),
            present_upscale,
        },
    )
    .map_err(|e| HostError::App(format!("the Vulkan backend: {e:?}")))?;
    // `vkGet*ProcAddr` hands out the big-stack stub of a function that has one; the loader wraps the imports itself.
    let mut stubs = std::collections::HashMap::new();
    for e in nxkit_rt::entries_for(nxkit_rt::Graphics::Vulkan) {
        if e.big_stack && e.name.starts_with("vk") {
            let stub = crate::bigstack::wrap(e.addr as u64)?;
            stubs.insert(e.addr, stub as usize);
        }
    }
    nxkit_rt::vk::install(
        backend.clone() as Arc<dyn nxkit_vk::Backend>,
        nxkit_rt::vk::Settings {
            stubs,
            ..nxkit_rt::vk::Settings::default()
        },
    )
    .map_err(|e| HostError::App(format!("the Vulkan front end: {e:?}")))?;
    let pacer = Pacer::new(present_plan(), backend.clone());
    backend.set_pacer(pacer.clone());
    Ok(ticks(pacer, refresh))
}

/// The present plan of a game without frame step data: 60 game frames on the 120 Hz link.
fn present_plan() -> nxkit_metal::PresentPlan {
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
    plan
}

fn ticks(pacer: Arc<Pacer>, refresh: &Arc<crate::refresh::Refresh>) -> Arc<dyn FrameHooks> {
    Arc::new(Ticks {
        pacer,
        refresh: refresh.clone(),
        applied: Mutex::new((0, 0)),
        rate: Mutex::new(RateTracker::new()),
        ticks: AtomicU64::new(0),
        seen: AtomicU64::new(0),
    })
}
