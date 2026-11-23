//! The seams: every place where the host needs an API that another crate builds.
//!
//! A seam is a function in this file. A seam that cannot work returns `HostError::Unwired` with the name of what is
//! missing, and the host stops at start with that message (R15). `docs/nxkit-host.md` lists the seams and what each
//! one does.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use nxkit_abi::ipc::Named;
use nxkit_abi::launch;
use nxkit_metal::{Context, FrameHooks};
use nxkit_rt::Service;
use nxkit_svc_input::{KeyMap, PadMap};
use nxkit_svc_pacing::{LoadConfig, StepMode};

use crate::error::{HostError, Result};
use crate::manifest::{self, Game, Import};
use crate::refresh::Refresh;
use crate::settings::Settings;
use crate::sys::appkit::{MouseEvent, MouseMode};
use crate::{guestsym, saveplace, wire};

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

/// The names of the exports of the selected services in `table` (`nxkit_rt::entries_for` the graphics API of the
/// game) that run on a big stack. The loader puts a big-stack stub (`bigstack.rs`) into the import slots of these.
pub fn big_stack_exports(table: &[nxkit_rt::Entry], mask: u64) -> HashSet<String> {
    table
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

/// The save and temporary folders of the game (`saveplace.rs`). The save that an older build kept in the run folder
/// moves to the new place first, once.
fn save_places(game: &Game, env: &crate::hostenv::HostEnv) -> Result<saveplace::Places> {
    let places = saveplace::places(
        env.home.as_deref(),
        env.save.as_deref(),
        &game.root,
        title_id(game)?,
        &game.title_id,
    );
    match saveplace::migrate(&places.bundled, &places.save) {
        saveplace::Migration::Nothing => {}
        saveplace::Migration::Moved => eprintln!(
            "nxkit-host: the save moved from the app {} to {}",
            places.bundled.display(),
            places.save.display()
        ),
        saveplace::Migration::Kept(why) => eprintln!(
            "nxkit-host: the save in the app {} stays there and is not used: {why}",
            places.bundled.display()
        ),
    }
    eprintln!("nxkit-host: save {}", places.save.display());
    Ok(places)
}

/// The folder that holds the `<index>` folders of the DLC.
fn aoc_root(game: &Game) -> Option<PathBuf> {
    game.aoc_dirs()
        .first()
        .and_then(|(_, dir)| dir.parent().map(Path::to_path_buf))
}

/// Whether the device save has its own folder. That is the case when the main module imports
/// `nn::fs::MountSaveData`, so the game mounts an account save next to its device save (R35).
fn separate_device_save(game: &Game) -> Result<bool> {
    let main = game.main_module()?;
    let imports = manifest::read_imports(&game.path(&main.imports))?;
    Ok(imports_account_save(&imports))
}

fn imports_account_save(imports: &[Import]) -> bool {
    imports
        .iter()
        .any(|i| i.name.starts_with("_ZN2nn2fs13MountSaveData"))
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

/// The frame limits of the main module (R30b): the rules of `game.json` and the offsets that the fixups of the main
/// module hold for their keys, with the title's target. `None` when the title has no rule.
fn limits_config(game: &Game) -> Option<nxkit_svc_pacing::LimitsConfig> {
    let rules = &game.limits;
    if rules.is_empty() {
        return None;
    }
    let main = game.main_module().ok()?;
    let module = crate::limits::module_of(main, rules.found_keys(), |k| game.main_fixup(k));
    Some(nxkit_svc_pacing::LimitsConfig {
        module,
        target: game.settings.fps_target,
        rows: rules.rows.clone(),
        reflected: rules.reflected,
    })
}

/// Whether a limit rule of the title can take the target: a row whose place the fixups name, a row of a build or a
/// reflected class. The pacing service checks the build and the class when it starts.
fn has_limit(cfg: Option<&nxkit_svc_pacing::LimitsConfig>) -> bool {
    cfg.is_some_and(|c| {
        c.reflected.is_some()
            || c.rows.iter().any(|r| match r.site {
                nxkit_svc_pacing::limits::Site::Found { key, .. } => {
                    c.module.found.iter().any(|f| f.key == key)
                }
                nxkit_svc_pacing::limits::Site::Fixed { .. } => true,
            })
    })
}

/// Whether the game runs at 120 fps through its engine's frame limit (R30b): a target of 120, `fps_max` of 120, no frame
/// step state page and a limit rule. The refresh plans the link and the vsync at 120 then.
pub fn runs_at_120(game: &Game) -> bool {
    let (high, why) = high(game);
    if let Some(n) = why {
        eprintln!("nxkit-host: {}", n.text);
    }
    high
}

fn high(game: &Game) -> (bool, Option<nxkit_svc_pacing::Notice>) {
    let s = &game.settings;
    nxkit_svc_pacing::interval::runs_at_120(
        s.fps_target,
        s.fps_max,
        state_page(game).is_some(),
        has_limit(limits_config(game).as_ref()),
    )
}

/// Whether the host can make the game run at `target` fps: through the frame step state page that the game's frame step
/// patch writes, or through a frame limit of its engine that a rule of the title names. Without either a load-driven step
/// can only hold the presents longer, which the game does not see: a game that paces itself (Octopath Traveler at 30
/// with a target of 60) then had its hold flip between 1, 2 and 3 vsyncs. 0 asks for nothing.
fn target_enforceable(target: u32, has_page: bool, has_limit: bool) -> bool {
    target == 0 || has_page || has_limit
}

fn pacing_config(game: &Game, refresh: &Refresh) -> Result<nxkit_svc_pacing::Config> {
    let s = &game.settings;
    let page = state_page(game);
    let target = s.fps_target;
    let limits = limits_config(game);
    let limit = has_limit(limits.as_ref());
    let mut variable = None;
    if !target_enforceable(target, page.is_some(), limit) {
        eprintln!(
            "nxkit-host: fps_target {target} has no way to be enforced (no frame step state page, no limit rule): the game runs at its own pace"
        );
    }
    let step = match (target, page.is_some()) {
        // 120 through the engine's limit: the load-driven step without a page counts ticks of 1/120 s and decides the
        // hold, and the limits write 120 or 60 by it.
        (120, false) if high(game).0 => {
            let mut load = LoadConfig::new(120).map_err(|e| HostError::Manifest(e.to_string()))?;
            load.hold = s.fps_hold;
            StepMode::Load(load)
        }
        (0, _) | (_, false) => StepMode::Own,
        // The load-driven step decides the hold from the cost of the frames and writes the step into the state
        // page, so the game steps by the time each frame stays on screen (the engine's frame step).
        (30 | 60, true) => {
            let mut load =
                LoadConfig::new(target).map_err(|e| HostError::Manifest(e.to_string()))?;
            load.hold = s.fps_hold;
            load.work = matches!(s.pace_profile.as_str(), "work" | "steady");
            load.steady = s.pace_profile == "steady";
            StepMode::Load(load)
        }
        // A target above 60 asks for the variable step, which only works under variable refresh. The game starts on the
        // load-driven step at 60 and the refresh module switches when the screen allows (`Refresh::set_variable_step`).
        (t, true) if t > 60 => {
            let mut load = LoadConfig::new(60).map_err(|e| HostError::Manifest(e.to_string()))?;
            load.hold = s.fps_hold;
            load.work = matches!(s.pace_profile.as_str(), "work" | "steady");
            load.steady = s.pace_profile == "steady";
            variable = Some(t);
            StepMode::Load(load)
        }
        (t, true) => {
            let mut load = LoadConfig::new(t).map_err(|e| HostError::Manifest(e.to_string()))?;
            load.hold = s.fps_hold;
            load.work = matches!(s.pace_profile.as_str(), "work" | "steady");
            load.steady = s.pace_profile == "steady";
            StepMode::Load(load)
        }
    };
    // Variable refresh needs to know that the game steps by the measured frame time.
    refresh.set_variable_step(variable, step);
    Ok(nxkit_svc_pacing::Config {
        bind_system_event: wire::pacing_bind,
        state_page: page,
        // Start values. The window sets the real link rate from the refresh rate of its screen (`refresh.rs`), the
        // Switch's vsync is 60 Hz.
        display_hz: 120.0,
        vsync_hz: 60.0,
        step,
        limits,
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
fn console_language(vars: &crate::hostenv::HostEnv) -> String {
    let env = vars.language.clone();
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

/// The map of the variable `name` from its `value`. A value that does not parse whole is ignored with one line in the
/// log, and the default stays.
fn input_map<T: Default, E: std::fmt::Display>(
    name: &str,
    value: Option<&str>,
    parse: impl FnOnce(&str) -> std::result::Result<T, E>,
) -> T {
    let Some(value) = value else {
        return T::default();
    };
    parse(value).unwrap_or_else(|e| {
        eprintln!("nxkit-host: {name}: {e}, the value is ignored");
        T::default()
    })
}

/// Starts the service crates with the configuration of the game. The services are the names in `game.services`.
///
/// Every crate gets typed values from `game.json`: fs the romfs, the title ID and the DLC folder, system the launch
/// parameter, the display version, the shared font facts and the DLC folder, input the pad layout, the focus and the
/// input files (`input_file.rs`), audio and video their host seams and pacing the step mode with the state page. The
/// lookup of the game's own symbols is set in all of them (`guestsym.rs`).
pub fn init_services(
    game: &Game,
    mask: u64,
    env: &crate::hostenv::HostEnv,
    refresh: &Refresh,
) -> Result<()> {
    let s = &game.settings;
    let handheld = s.op_mode == "handheld";
    wire::set_handheld(handheld);
    guestsym::register();
    nxkit_hos::init_os(nxkit_hos::OsConfig {
        game_env: game_env(s),
        alloc_big_align: game.memory.alloc_big_align,
    })
    .map_err(HostError::App)?;

    let places = save_places(game, env)?;
    // The account service owns the users, so it starts first: the fs service asks it for the user of a uid.
    let system = has(mask, Service::System);
    if system {
        let fonts = game.root.join("font");
        nxkit_svc_system::init(&nxkit_svc_system::Config {
            nickname: env.nick.clone().unwrap_or_else(|| "Player".to_owned()),
            users_file: Some(saveplace::sibling(&places.save, ".users")),
            launch_user: env.user,
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
            language: console_language(env),
            ..nxkit_svc_system::Config::default()
        });
        nxkit_svc_system::set_seams(wire::system_seams());
    }
    if has(mask, Service::Fs) {
        for dir in [&places.save, &places.temp] {
            std::fs::create_dir_all(dir).map_err(|e| HostError::io(dir, &e))?;
        }
        nxkit_svc_fs::init(&nxkit_svc_fs::Config {
            title_id: title_id(game)?,
            romfs: game.path(&game.romfs),
            save: places.save.clone(),
            temp: places.temp,
            aoc: aoc_root(game),
            separate_device_save: separate_device_save(game)?,
            user_of: system.then_some(nxkit_svc_system::user_of as nxkit_svc_fs::UserOf),
            hooks: Some(crate::savesync::start(crate::savesync::config(
                &places.save,
                title_id(game)?,
                env,
                Arc::new(nxkit_svc_fs::clone_committed),
            ))),
        });
    }
    if has(mask, Service::Input) {
        let mut config = nxkit_svc_input::Config {
            handheld,
            pad_layout: s.pad_layout.clone(),
            rumble: env.rumble,
            rumble_gain: env.rumble_gain,
            rumble_min: env.rumble_min,
            hd_rumble: env.hd_rumble,
            stick_deadzone_l: env.stick_deadzone_l,
            stick_deadzone_r: env.stick_deadzone_r,
            pad_merge: env.pad_merge,
            pad_press_to_join: env.pad_press_to_join,
            pad_map: input_map(launch::PAD_MAP, env.pad_map.as_deref(), PadMap::parse),
            key_map: input_map(launch::KEYMAP, env.key_map.as_deref(), KeyMap::parse),
            ..nxkit_svc_input::Config::default()
        };
        for code in config.key_map.unread_codes() {
            eprintln!(
                "nxkit-host: {}: key code {code} is a key that nxkit does not read, its function is on no key",
                launch::KEYMAP
            );
        }
        let (file, warnings) = crate::input_file::load(&game.root, &game.title_id);
        for w in warnings {
            eprintln!("nxkit-host: input: {w}");
        }
        file.apply_to(&mut config);
        nxkit_svc_input::init(&config);
        nxkit_svc_input::set_system_event_hooks(wire::input_hooks());
    }
    if has(mask, Service::Audio) {
        nxkit_svc_audio::init(&nxkit_svc_audio::Config {
            spatial_audio: s.spatial_audio,
            ..nxkit_svc_audio::Config::default()
        });
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
            // The video service writes decoded pictures into native images itself. The textures over them load again.
            guest_write: Some(nxkit_rt::nvn::guest_wrote),
        });
    }
    if has(mask, Service::Pacing) {
        nxkit_svc_pacing::init(&pacing_config(game, refresh)?)
            .map_err(|e| HostError::Manifest(format!("pacing: {e:?}")))?;
        for n in nxkit_svc_pacing::notices().unwrap_or_default() {
            eprintln!("nxkit-pacing: {}", n.text);
        }
    }
    Ok(())
}

/// The window gained or lost the key focus. The input service releases held keys on a loss. The game keeps its focus:
/// on the console a focus loss means the HOME menu, and games pause for it, but on a Mac clicking another window is no
/// reason to stop the game. `NXKIT_PAUSE_UNFOCUSED=1` reports the loss to the game as before.
pub fn focus_changed(focused: bool) {
    let pause = crate::hostenv::host().pause_unfocused;
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

/// The buttons of every controller for the Home button, as the input service reads them apart from the game.
pub fn home_buttons() -> crate::home::Buttons {
    let b = nxkit_svc_input::system_buttons();
    crate::home::Buttons {
        home: b.home,
        a: b.a,
        b: b.b,
        up: b.up,
        down: b.down,
        left: b.left,
        right: b.right,
        x: b.x,
        y: b.y,
        plus: b.plus,
        minus: b.minus,
        l: b.l,
        r: b.r,
        connected: b.connected,
    }
}

/// The focus a screen of the Home button gives the game: out of focus under the menu and the prompt, in the
/// background while it is paused behind the launcher (the console's HOME menu and a suspended application).
pub fn game_focus(focus: crate::home::GameFocus) {
    let focus = match focus {
        crate::home::GameFocus::In => nxkit_svc_system::Focus::InFocus,
        crate::home::GameFocus::Out => nxkit_svc_system::Focus::OutOfFocus,
        crate::home::GameFocus::Background => nxkit_svc_system::Focus::Background,
    };
    nxkit_svc_system::set_focus(focus);
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
    memory: &nxkit_gpu::GuestBuffer,
    root: &std::path::Path,
    settings: &Settings,
    run: &crate::hostenv::RunSettings,
    screen: &Arc<nxkit_nvn::screen::Screen>,
    refresh: &Arc<Refresh>,
) -> Result<Arc<dyn FrameHooks>> {
    crate::frames::install(ctx, memory, root, settings, run, screen, refresh)
}

/// The hooks of the display link for a game on Vulkan: the backend of `nxkit-vk-gpu` behind the Vulkan front end of
/// `nxkit-rt`, with the same pacer. The work is in `frames.rs`.
pub fn vulkan_frame_hooks(
    ctx: &Context,
    root: &std::path::Path,
    settings: &Settings,
    run: &crate::hostenv::RunSettings,
    refresh: &Arc<Refresh>,
) -> Result<Arc<dyn FrameHooks>> {
    crate::frames::install_vulkan(ctx, root, settings, refresh, run.host.present_upscale)
}

#[cfg(test)]
mod tests {
    use super::{imports_account_save, target_enforceable};
    use crate::manifest::parse_imports;

    fn imports(names: &[&str]) -> Vec<crate::manifest::Import> {
        let list: Vec<String> = names
            .iter()
            .map(|n| format!(r#"{{"name": "{n}", "slot": 0, "addend": 0, "weak": false}}"#))
            .collect();
        parse_imports(&format!("[{}]", list.join(","))).unwrap()
    }

    #[test]
    fn the_device_save_is_separate_only_next_to_an_account_save() {
        let device = "_ZN2nn2fs19MountDeviceSaveDataEPKc";
        let account = "_ZN2nn2fs13MountSaveDataEPKcRKNS_7account3UidE";
        assert!(imports_account_save(&imports(&[device, account])));
        assert!(!imports_account_save(&imports(&[device])));
    }

    #[test]
    fn a_target_needs_the_frame_step_page() {
        assert!(target_enforceable(0, false, false));
        assert!(target_enforceable(60, true, false));
        assert!(target_enforceable(60, false, true), "a limit rule");
        assert!(!target_enforceable(60, false, false));
        assert!(!target_enforceable(30, false, false));
    }
}
