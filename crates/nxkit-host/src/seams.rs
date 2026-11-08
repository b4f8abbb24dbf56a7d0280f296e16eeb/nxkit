//! The seams: every place where the host needs an API that another crate builds.
//!
//! A seam is a function in this file. A seam that cannot work returns `HostError::Unwired` with the name of what is
//! missing, and the host stops at start with that message (R15). `docs/nxkit-host.md` lists the seams and what each
//! one does.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use nxkit_abi::ipc::Named;
use nxkit_metal::{Context, FrameHooks, GuestMemory};
use nxkit_rt::Service;
use nxkit_svc_pacing::{LoadConfig, StepMode};

use crate::error::{HostError, Result};
use crate::manifest::Game;
use crate::settings::Settings;
use crate::sys::appkit::{MouseEvent, MouseMode};
use crate::{guestsym, wire};

fn has(mask: u64, service: Service) -> bool {
    mask & service.bit() != 0
}

/// Checks that the kernel that `nxkit_kernel_init` made is registered with `nxkit_abi::kernel::set_kernel`, so that
/// the services can ask for the current thread, wait and signal. `nxkit_kernel_init` does the registration. The
/// regions of the reservation, the memory profile and the core rules go in through `sys::kernel_init`.
pub fn register_kernel() -> Result<()> {
    if nxkit_abi::kernel::kernel().is_none() || nxkit_rt::kernel().is_none() {
        return Err(HostError::Unwired(
            "the kernel is not registered with nxkit_abi::kernel::set_kernel",
        ));
    }
    Ok(())
}

/// Makes the calling host thread the main thread of the guest, with the priority, the core and the stack size of the
/// NPDM that the kernel got at its start. It returns the kernel handle of the main thread.
pub fn attach_main_thread() -> Result<u32> {
    nxkit_rt::attach_main_thread().map_err(HostError::App)
}

/// Builds the IPC server from the `services()` of every selected service crate and registers it with
/// `nxkit_abi::kernel::set_server`, so that `SendSyncRequest` reaches it.
pub fn register_server(mask: u64) -> Result<()> {
    let mut named: Vec<Named> = Vec::new();
    for (service, services) in [
        (Service::Fs, nxkit_svc_fs::services as fn() -> Vec<Named>),
        (Service::Offline, nxkit_svc_offline::services),
        (Service::Input, nxkit_svc_input::services),
        (Service::Audio, nxkit_svc_audio::services),
        (Service::Video, nxkit_svc_video::services),
        (Service::System, nxkit_svc_system::services),
        (Service::Pacing, nxkit_svc_pacing::services),
    ] {
        if has(mask, service) {
            named.extend(services());
        }
    }
    let ipc = nxkit_ipc::Ipc::new(named).map_err(|e| HostError::Manifest(e.to_string()))?;
    if !nxkit_abi::kernel::set_server(ipc.server()) {
        return Err(HostError::Manifest(
            "the IPC server is registered twice".into(),
        ));
    }
    Ok(())
}

/// The names of the exports of the selected services that run on a big stack. The loader puts a big-stack stub
/// (`bigstack.rs`) into the import slots of these.
pub fn big_stack_exports(mask: u64) -> HashSet<String> {
    nxkit_rt::entries()
        .iter()
        .filter(|e| e.big_stack && has(mask, e.service))
        .map(|e| e.name.to_owned())
        .collect()
}

fn title_id(game: &Game) -> Result<u64> {
    u64::from_str_radix(game.title_id.trim_start_matches("0x"), 16).map_err(|_| {
        HostError::Manifest(format!(
            "\"title_id\" {} is not a hex number",
            game.title_id
        ))
    })
}

/// The folder that holds the `<index>` folders of the DLC.
fn aoc_root(game: &Game) -> Option<PathBuf> {
    game.aoc_dirs()
        .first()
        .and_then(|(_, dir)| dir.parent().map(Path::to_path_buf))
}

/// The version that the game shows for the build ID of its main module.
fn display_version(game: &Game) -> String {
    let id = game
        .main_module()
        .map(|m| m.build_id.to_ascii_lowercase())
        .unwrap_or_default();
    game.settings
        .display_versions
        .iter()
        .find(|(build, _)| id.starts_with(&build.to_ascii_lowercase()))
        .map_or_else(|| game.version.clone(), |(_, v)| v.clone())
}

/// The guest address of the frame step page, from the `FPS_STATE_OFF` line of the fixups of the main module.
fn state_page(game: &Game) -> Option<u64> {
    let main = game.main_module().ok()?;
    let text = std::fs::read_to_string(game.path(&main.fixups)).ok()?;
    let line = text
        .lines()
        .find_map(|l| l.strip_prefix("# FPS_STATE_OFF\t"))?;
    let offset = u64::from_str_radix(line.trim().trim_start_matches("0x"), 16).ok()?;
    main.base.checked_add(offset)
}

fn pacing_config(game: &Game) -> Result<nxkit_svc_pacing::Config> {
    let s = &game.settings;
    let page = state_page(game);
    let target = s.fps_target;
    let step = match (target, page.is_some()) {
        (0, _) => StepMode::Own,
        // The load-driven step decides the hold from the cost of the frames and writes the step into the state
        // page, so the game steps by the time each frame stays on screen (nxnative's frame step).
        (30 | 60, true) => {
            let mut load =
                LoadConfig::new(target).map_err(|e| HostError::Manifest(e.to_string()))?;
            load.hold = s.fps_hold;
            StepMode::Load(load)
        }
        (t, true) if t > 60 => StepMode::Variable(t),
        (t, _) => {
            let mut load = LoadConfig::new(t).map_err(|e| HostError::Manifest(e.to_string()))?;
            load.hold = s.fps_hold;
            StepMode::Load(load)
        }
    };
    Ok(nxkit_svc_pacing::Config {
        bind_system_event: wire::pacing_bind,
        state_page: page,
        // Start values. The window sets the real link rate from the refresh rate of its screen (`refresh.rs`), the
        // Switch's vsync is 60 Hz.
        display_hz: 120.0,
        vsync_hz: 60.0,
        step,
        limits: None,
        display_mode: s.present_display,
    })
}

/// The `KEY=VALUE` pairs of `settings.env`, separated by white space.
fn game_env(settings: &Settings) -> Vec<(String, String)> {
    settings
        .env
        .split_whitespace()
        .filter_map(|pair| {
            pair.split_once('=')
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
        })
        .collect()
}

/// The console language: `NXKIT_LANGUAGE` (a tag such as `de` or `en-GB`) when it is set, else the first of the Mac's
/// preferred languages that the console has, else `en-US`.
fn console_language() -> String {
    let env = std::env::var("NXKIT_LANGUAGE")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let mac = crate::sys::preferred_languages();
    let tags: Vec<&str> = env
        .as_deref()
        .into_iter()
        .chain(mac.iter().map(String::as_str))
        .collect();
    let lang = nxkit_svc_system::console_language(tags.iter().copied());
    eprintln!("nxkit-host: console language {lang} (preferred {tags:?})");
    lang.to_owned()
}

/// Starts the service crates with the configuration of the game. The services are the names in `game.services`.
///
/// Every crate gets typed values from `game.json`: fs the romfs, the title ID and the DLC folder, system the launch
/// parameter, the display version, the shared font facts and the DLC folder, input the pad layout, the focus and the
/// input files (`input_file.rs`), audio and video their host seams and pacing the step mode with the state page. The
/// lookup of the game's own symbols is set in all of them (`guestsym.rs`).
pub fn init_services(game: &Game, mask: u64) -> Result<()> {
    let s = &game.settings;
    let handheld = s.op_mode == "handheld";
    wire::set_handheld(handheld);
    guestsym::register();
    nxkit_hos::init_os(nxkit_hos::OsConfig {
        game_env: game_env(s),
        alloc_big_align: game.memory.alloc_big_align,
    })
    .map_err(HostError::App)?;

    if has(mask, Service::Fs) {
        let save = game.root.join("save").join(&game.title_id);
        let temp = game.root.join("temp");
        for dir in [&save, &temp] {
            std::fs::create_dir_all(dir).map_err(|e| HostError::io(dir, &e))?;
        }
        nxkit_svc_fs::init(&nxkit_svc_fs::Config {
            title_id: title_id(game)?,
            romfs: game.path(&game.romfs),
            save,
            temp,
            aoc: aoc_root(game),
            separate_device_save: false,
        });
    }
    if has(mask, Service::System) {
        let fonts = game.root.join("font");
        nxkit_svc_system::init(&nxkit_svc_system::Config {
            handheld,
            boost: s.perf_boost,
            launch_parameter: (!s.launch_param.is_empty())
                .then(|| s.launch_param.clone().into_bytes()),
            display_version: display_version(game),
            aoc_dir: aoc_root(game),
            font_dir: fonts.is_dir().then_some(fonts),
            shared_font: s
                .shared_font
                .as_ref()
                .map(|f| game.path(Path::new(f)))
                .filter(|p| p.is_file()),
            shared_font_decode: s.shared_font_decode,
            sf_services: s.sf_services.clone(),
            language: console_language(),
            ..nxkit_svc_system::Config::default()
        });
        nxkit_svc_system::set_seams(wire::system_seams());
    }
    if has(mask, Service::Input) {
        let mut config = nxkit_svc_input::Config {
            handheld,
            pad_layout: s.pad_layout.clone(),
            ..nxkit_svc_input::Config::default()
        };
        let (file, warnings) = crate::input_file::load(&game.root, &game.title_id);
        for w in warnings {
            eprintln!("nxkit-host: input: {w}");
        }
        file.apply_to(&mut config);
        nxkit_svc_input::init(&config);
        nxkit_svc_input::set_system_event_hooks(wire::input_hooks());
    }
    if has(mask, Service::Audio) {
        nxkit_svc_audio::init(&nxkit_svc_audio::Config::default());
        if !nxkit_svc_audio::set_host(wire::audio_host()) {
            return Err(HostError::Manifest(
                "the audio host seams are set twice".into(),
            ));
        }
    }
    if has(mask, Service::Video) {
        nxkit_svc_video::init(&nxkit_svc_video::Config {
            host: Arc::new(wire::VideoHost),
            sink: None,
        });
    }
    if has(mask, Service::Pacing) {
        nxkit_svc_pacing::init(&pacing_config(game)?)
            .map_err(|e| HostError::Manifest(format!("pacing: {e:?}")))?;
    }
    Ok(())
}

/// The window gained or lost the key focus. The input service releases held keys on a loss. The game keeps its focus:
/// on the console a focus loss means the HOME menu, and games pause for it, but on a Mac clicking another window is no
/// reason to stop the game. `NXKIT_PAUSE_UNFOCUSED=1` reports the loss to the game as before.
pub fn focus_changed(focused: bool) {
    static PAUSE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let pause =
        *PAUSE.get_or_init(|| std::env::var("NXKIT_PAUSE_UNFOCUSED").is_ok_and(|v| v == "1"));
    if pause || focused {
        let focus = if focused {
            nxkit_svc_system::Focus::InFocus
        } else {
            nxkit_svc_system::Focus::OutOfFocus
        };
        nxkit_svc_system::set_focus(focus);
    }
    nxkit_svc_input::set_focus(focused);
}

/// The held mouse did something. The input service turns it into the camera stick and the mouse bindings.
pub fn mouse(event: MouseEvent) {
    match event {
        MouseEvent::Moved { dx, dy } => nxkit_svc_input::mouse_moved(dx, dy),
        MouseEvent::Button { number, down } => nxkit_svc_input::mouse_button(number, down),
        MouseEvent::Wheel { dy } => nxkit_svc_input::mouse_wheel(dy),
        MouseEvent::Released => nxkit_svc_input::mouse_release(),
    }
}

/// When the window holds the mouse: never when the input files turn the mouse off, on a click while a controller is
/// connected, else whenever the window has the focus (R63).
pub fn mouse_mode() -> MouseMode {
    if !nxkit_svc_input::mouse_capture() {
        MouseMode::Off
    } else if nxkit_svc_input::controller_connected() {
        MouseMode::Click
    } else {
        MouseMode::Auto
    }
}

/// The hooks that the display link calls. They connect the encoder of `nxkit-gpu` and the pacing of `nxkit-metal`
/// to the window: on each display tick they present the frame that the guest finished. `settings` has the pacing and
/// encoder keys (`fps_target`, `fps_max`, `fps_hold`, `present_snap`, `pso_mtl4`, `enc_thread` and the `tex_*` keys).
///
/// `root` is the run folder, which has the shaders. The work is in `frames.rs`.
pub fn frame_hooks(
    ctx: &Context,
    memory: &Arc<GuestMemory>,
    root: &std::path::Path,
    settings: &Settings,
) -> Result<Arc<dyn FrameHooks>> {
    crate::frames::install(ctx, memory, root, settings)
}
