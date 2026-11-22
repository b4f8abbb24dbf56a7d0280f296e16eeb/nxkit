//! The settings of the run from the `NXKIT_*` environment (W27).
//!
//! [`HostEnv`] has every key that the host itself reads, its default and its parsing. [`RunSettings`] holds the typed
//! settings of every runtime crate, each built by the crate's own `from_env` from the one snapshot that `run` takes.
//! [`RunSettings::install`] gives the crates that keep their settings for code with no context (the NVN front end, the
//! encoder, the services) their settings, and the host passes the rest explicitly when it builds the parts.
//!
//! The launch options of `nxkit_abi::launch` are keys of this file, read by the names of that table. [`with_saved`]
//! adds the saved choices of the player to the snapshot before anything reads it.

use std::path::PathBuf;

use nxkit_abi::env::{Env, Slot, SlotError};
use nxkit_abi::launch;

/// The settings of the host itself.
#[derive(Clone, Debug, PartialEq)]
pub struct HostEnv {
    /// `NXKIT_MEM_RESERVE` as given: the heap region in place of the title's (a size as `parse_size` reads it).
    pub mem_reserve: Option<String>,
    /// `NXKIT_MEM_ALIAS` as given: the alias region in place of the title's.
    pub mem_alias: Option<String>,
    /// `NXKIT_STALL_DUMP` (seconds above 0): with no present on screen for that long, the report of every guest
    /// thread is printed, once per stall.
    pub stall_dump: Option<f64>,
    /// `NXKIT_GUEST_KEEP` (on unless `0`): the kernel pins the pages under the GPU buffer.
    pub guest_keep: bool,
    /// `NXKIT_GUEST_WIRE` (off unless set and not `0`): the guest range is wired with `mlock`.
    pub guest_wire: bool,
    /// `NXKIT_PIPELINE_CACHE_DIR` (not empty): the folder of the runtime pipeline cache.
    pub pipeline_cache_dir: Option<PathBuf>,
    /// `NXKIT_SET_REFRESH`: the refresh rate to set, over the `set_refresh` key.
    pub set_refresh: Option<u32>,
    /// `NXKIT_TIMED_PRESENT` (`1` or `0`): timed presents on or off, over the plan.
    pub timed_present: Option<bool>,
    /// `NXKIT_VRR` (on unless `0`): variable refresh where the screen has it.
    pub vrr: bool,
    /// `NXKIT_VRR_MIN` (above 0): the lowest rate of the variable refresh.
    pub vrr_min: Option<u32>,
    /// `NXKIT_ARCHIVE_SHARDS` is `0` or `off`: the shards are ignored and the single archive is loaded.
    pub archive_shards_off: bool,
    /// `NXKIT_SHARD_LRU`: the shards that stay open, 0 for the default of 32.
    pub shard_lru: usize,
    /// `NXKIT_ENCODE_THREADS`: the encode threads, 0 picks them from the cores.
    pub encode_threads: usize,
    /// `NXKIT_SCALE` (1.0): the render scale.
    pub scale: f32,
    /// `NXKIT_LANGUAGE` (not blank): the console language tag.
    pub language: Option<String>,
    /// `NXKIT_MEM_REPORT`: the seconds between two memory reports, when it is a number.
    pub mem_report: Option<f64>,
    /// `NXKIT_CRASH_REPORT=0`: no crash handler.
    pub crash_report_off: bool,
    /// `NXKIT_CONTROL` (on unless `0`): the control socket of the run (`control.rs`, W35).
    pub control: bool,
    /// `NXKIT_PAUSE_UNFOCUSED=1`: the game pauses when the window loses the focus.
    pub pause_unfocused: bool,
    /// `NXKIT_OP_MODE` (`docked` or `handheld`, a launch option): handheld (`true`) or docked over the mode of the
    /// title module. Another value is ignored.
    pub handheld: Option<bool>,
    /// `NXKIT_RUMBLE` (on unless `0`, a launch option): the controllers rumble.
    pub rumble: bool,
    /// `NXKIT_RUMBLE_GAIN` (0 to 3, 1.0 otherwise, a launch option): the factor on the rumble of the game.
    pub rumble_gain: f32,
    /// `NXKIT_RUMBLE_MIN` (0 to 0.9, 0 otherwise, a launch option): the rumble below which a controller does not move.
    pub rumble_min: f32,
    /// `NXKIT_HD_RUMBLE` (on unless `0`, a launch option): a Switch Pro Controller gets HD Rumble.
    pub hd_rumble: bool,
    /// `NXKIT_METAL_HUD=1` (a launch option): the Metal performance HUD over the game.
    pub metal_hud: bool,
    /// `NXKIT_PRESENT_UPSCALE` (`metalfx` or `1` for MetalFX, off otherwise, a launch option): how the present scales a
    /// picture that is smaller than the window.
    pub present_upscale: nxkit_metal::PresentUpscale,
    /// `NXKIT_FPS_TARGET` (a whole number, a launch option): the frame rate over `fps_target` of the title module.
    /// `Settings::with_env` keeps it only when the title offers it.
    pub fps_target: Option<u32>,
    /// `NXKIT_RENDER_SIZE` (a launch option): the height of the size the game renders at, `Some(0)` for the game's own
    /// size, over `render_size` of the title module. `None`: not set or not an option. It takes effect for a game whose
    /// `game.json` has `render_args`.
    pub render_size: Option<u32>,
    /// `NXKIT_STICK_DEADZONE_L` (0 to 50, a whole number, 0 otherwise, a launch option): the dead zone of the left
    /// stick of a controller in percent.
    pub stick_deadzone_l: u8,
    /// `NXKIT_STICK_DEADZONE_R` (as the left one, a launch option): the dead zone of the right stick.
    pub stick_deadzone_r: u8,
    /// `NXKIT_PAD_MERGE=1` (a launch option): every controller is player 1.
    pub pad_merge: bool,
    /// `NXKIT_PAD_PRESS_TO_JOIN=1` (a launch option): a new controller becomes a player at its first button press.
    pub pad_press_to_join: bool,
    /// `NXKIT_PAD_MAP` (not blank, set by the launcher, no launch option): what the buttons of a controller send, as
    /// given. `nxkit_svc_input::PadMap::parse` reads it.
    pub pad_map: Option<String>,
    /// `NXKIT_KEYMAP` (not blank, set by the launcher, no launch option): functions on other keys, as given.
    /// `nxkit_svc_input::KeyMap::parse` reads it.
    pub key_map: Option<String>,
    /// `NXKIT_SCREEN`: the screen of the window, by index, `id:` and its display id, or a part of its name.
    pub screen: Option<String>,
    /// `NXKIT_FULLSCREEN` (off unless set, not blank and not `0`): the window starts in full screen, as
    /// `--fullscreen`.
    pub fullscreen: bool,
    /// `NXKIT_HOME_MENU` (on unless `0`): a short press of a controller's Home button opens the Home menu.
    pub home_menu: bool,
    /// `NXKIT_HOME_QUIT` (on unless `0`): holding Home for 2 seconds opens the quit prompt.
    pub home_quit: bool,
    /// `NXKIT_LAUNCHER_BUNDLE_ID` (not blank): the launcher that started the game. The Home menu gets Back to the
    /// launcher, which pauses the game and shows that application.
    pub launcher: Option<String>,
    /// `NXKIT_HOME_TEXT`: the texts of the Home button in the player's language, one per line (`home::Texts`).
    pub home_text: Option<String>,
    /// `NXKIT_LAUNCHER_PID`: the process of the launcher that started the game. Back to the launcher activates this
    /// process, so it reaches the right one when two copies of a launcher run. Else it reaches the first with the
    /// bundle id.
    pub launcher_pid: Option<i32>,
    /// `NXKIT_CURSOR_HIDE` (3): the seconds without movement after which the pointer over the game hides, `0` never.
    pub cursor_hide: f64,
    /// `NXKIT_CURSOR_HIDDEN=1`: the pointer starts hidden (the launcher's own was hidden when it started the game).
    pub cursor_hidden: bool,
    /// `NXKIT_OVERLAY=1`: the game opens over the launcher's full-screen Space instead of a Space of its own, fades in
    /// with its first picture and gives the launcher the progress of the start (`sys/present.rs`).
    pub overlay: bool,
    /// `NXKIT_SAVE` (not blank, set by the launcher, no launch option): the save folder of user 1 in place of the one
    /// under Application Support (`saveplace.rs`).
    pub save: Option<PathBuf>,
    /// `HOME`: the home folder, which holds `Library/Application Support/nxkit`.
    pub home: Option<PathBuf>,
    /// `NXKIT_USER` (1 to 8, set by the launcher, no launch option): the user the game starts with, as a slot from 0.
    /// Another value is user 1.
    pub user: usize,
    /// `NXKIT_NICK` (not blank, set by the launcher, no launch option): the nickname of user 1.
    pub nick: Option<String>,
    /// `NXKIT_SAVE_SYNC` (not blank, set by the launcher, no launch option): the cloud saves folder, `icloud` or `0`,
    /// as given. `nxkit-save-sync` reads it.
    pub save_sync: Option<String>,
    /// `NXKIT_SAVE_SYNC_CHOICE` (set by the launcher, no launch option): the player's choice, as given.
    pub save_sync_choice: Option<String>,
}

impl Default for HostEnv {
    fn default() -> Self {
        HostEnv::from_env(&Env::default())
    }
}

impl HostEnv {
    /// The settings from the snapshot of the host.
    pub fn from_env(env: &Env) -> Self {
        HostEnv {
            mem_reserve: env.var("NXKIT_MEM_RESERVE").ok(),
            mem_alias: env.var("NXKIT_MEM_ALIAS").ok(),
            stall_dump: env
                .var("NXKIT_STALL_DUMP")
                .ok()
                .and_then(|v| v.trim().parse::<f64>().ok()),
            guest_keep: env.var("NXKIT_GUEST_KEEP").map_or(true, |v| v != "0"),
            guest_wire: env.var("NXKIT_GUEST_WIRE").is_ok_and(|v| v != "0"),
            pipeline_cache_dir: env
                .var_os("NXKIT_PIPELINE_CACHE_DIR")
                .filter(|d| !d.is_empty())
                .map(PathBuf::from),
            set_refresh: env
                .var("NXKIT_SET_REFRESH")
                .ok()
                .and_then(|v| v.trim().parse::<u32>().ok()),
            timed_present: env
                .var("NXKIT_TIMED_PRESENT")
                .ok()
                .and_then(|v| match v.trim() {
                    "1" => Some(true),
                    "0" => Some(false),
                    _ => None,
                }),
            vrr: env.var("NXKIT_VRR").map_or(true, |v| v.trim() != "0"),
            vrr_min: env
                .var("NXKIT_VRR_MIN")
                .ok()
                .and_then(|v| v.trim().parse::<u32>().ok())
                .filter(|hz| *hz > 0),
            archive_shards_off: env
                .var("NXKIT_ARCHIVE_SHARDS")
                .is_ok_and(|v| v == "0" || v == "off"),
            shard_lru: parsed(env, "NXKIT_SHARD_LRU").unwrap_or(0),
            encode_threads: parsed(env, "NXKIT_ENCODE_THREADS").unwrap_or(0),
            scale: parsed(env, "NXKIT_SCALE").unwrap_or(1.0),
            language: env
                .var(launch::LANGUAGE)
                .ok()
                .filter(|v| !v.trim().is_empty()),
            mem_report: env
                .var("NXKIT_MEM_REPORT")
                .ok()
                .and_then(|v| v.trim().parse().ok()),
            crash_report_off: env.var("NXKIT_CRASH_REPORT").is_ok_and(|v| v == "0"),
            control: env.var("NXKIT_CONTROL").map_or(true, |v| v.trim() != "0"),
            pause_unfocused: env.var(launch::PAUSE_UNFOCUSED).is_ok_and(|v| v == "1"),
            rumble: env.var(launch::RUMBLE).map_or(true, |v| v.trim() != "0"),
            rumble_gain: in_range(env, launch::RUMBLE_GAIN, 0.0, 3.0).unwrap_or(1.0),
            rumble_min: in_range(env, launch::RUMBLE_MIN, 0.0, 0.9).unwrap_or(0.0),
            hd_rumble: env.var(launch::HD_RUMBLE).map_or(true, |v| v.trim() != "0"),
            metal_hud: env.var(launch::METAL_HUD).is_ok_and(|v| v.trim() == "1"),
            present_upscale: match env.var(launch::PRESENT_UPSCALE).as_deref().map(str::trim) {
                Ok("metalfx" | "1") => nxkit_metal::PresentUpscale::MetalFx,
                _ => nxkit_metal::PresentUpscale::Off,
            },
            fps_target: env
                .var(launch::FPS_TARGET)
                .ok()
                .and_then(|v| v.trim().parse::<u32>().ok()),
            render_size: env
                .var(launch::RENDER_SIZE)
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| launch::RENDER_SIZES.iter().any(|(o, _)| *o == v.as_str()))
                .and_then(|v| v.parse().ok()),
            stick_deadzone_l: percent(env, launch::STICK_DEADZONE_L),
            stick_deadzone_r: percent(env, launch::STICK_DEADZONE_R),
            pad_merge: env.var(launch::PAD_MERGE).is_ok_and(|v| v.trim() == "1"),
            pad_press_to_join: env
                .var(launch::PAD_PRESS_TO_JOIN)
                .is_ok_and(|v| v.trim() == "1"),
            pad_map: not_blank(env, launch::PAD_MAP),
            key_map: not_blank(env, launch::KEYMAP),
            handheld: env.var(launch::OP_MODE).ok().and_then(|v| match v.trim() {
                "handheld" => Some(true),
                "docked" => Some(false),
                _ => None,
            }),
            screen: env.var("NXKIT_SCREEN").ok(),
            fullscreen: env
                .var(launch::FULLSCREEN)
                .is_ok_and(|v| !v.trim().is_empty() && v.trim() != "0"),
            home_menu: env.var(launch::HOME_MENU).map_or(true, |v| v.trim() != "0"),
            home_quit: env.var(launch::HOME_QUIT).map_or(true, |v| v.trim() != "0"),
            launcher: env
                .var("NXKIT_LAUNCHER_BUNDLE_ID")
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty()),
            home_text: env.var("NXKIT_HOME_TEXT").ok(),
            launcher_pid: parsed::<i32>(env, "NXKIT_LAUNCHER_PID").filter(|p| *p > 0),
            cursor_hide: parsed::<f64>(env, "NXKIT_CURSOR_HIDE")
                .filter(|s| s.is_finite() && *s >= 0.0)
                .unwrap_or(3.0),
            cursor_hidden: env
                .var("NXKIT_CURSOR_HIDDEN")
                .is_ok_and(|v| v.trim() == "1"),
            overlay: env.var("NXKIT_OVERLAY").is_ok_and(|v| v.trim() == "1"),
            save: not_blank(env, launch::SAVE).map(PathBuf::from),
            home: env
                .var_os("HOME")
                .filter(|h| !h.is_empty())
                .map(PathBuf::from),
            user: env
                .var(launch::USER)
                .ok()
                .and_then(|v| v.trim().parse::<usize>().ok())
                .filter(|n| (1..=8).contains(n))
                .map_or(0, |n| n - 1),
            nick: not_blank(env, launch::NICK),
            save_sync: not_blank(env, launch::SAVE_SYNC).map(|v| v.trim().to_owned()),
            save_sync_choice: not_blank(env, launch::SAVE_SYNC_CHOICE).map(|v| v.trim().to_owned()),
        }
    }
}

/// Adds the saved choices of the player to `env`: for each launch option, `saved` gives the stored value of
/// `launcher.<env>` in the preferences of the app, and a value that the option accepts is used when the environment
/// does not set the variable. `NXKIT_SAVED_CHOICES=0` ignores the choices. Returns one line per choice for the log.
pub fn with_saved(env: &mut Env, saved: impl Fn(&str) -> Option<String>) -> Vec<String> {
    if env
        .var("NXKIT_SAVED_CHOICES")
        .is_ok_and(|v| v.trim() == "0")
    {
        return vec!["saved choices ignored (NXKIT_SAVED_CHOICES=0)".into()];
    }
    let mut log = Vec::new();
    for o in launch::OPTIONS {
        let Some(v) = saved(&launch::saved_key(o.env)) else {
            continue;
        };
        let name = o.env;
        if !o.accepts(&v) {
            log.push(format!(
                "saved choice {name}={v} ignored: the host does not offer it"
            ));
        } else if let Ok(set) = env.var(name) {
            log.push(format!(
                "saved choice {name}={v} not used: the environment sets {name}={set}"
            ));
        } else if env.fill(name, &v) {
            log.push(format!("{name}={v} (saved choice)"));
        }
    }
    log
}

/// The value of `key` as a number from `min` to `max`, None when it is unset, not a number or outside.
fn in_range(env: &Env, key: &str, min: f32, max: f32) -> Option<f32> {
    parsed::<f32>(env, key).filter(|v| (min..=max).contains(v))
}

/// The value of `key` as a whole percent from 0 to 50, 0 when it is unset, not a whole number or above 50.
fn percent(env: &Env, key: &str) -> u8 {
    parsed::<u8>(env, key).filter(|p| *p <= 50).unwrap_or(0)
}

/// The value of `key` as given, None when it is unset or blank.
fn not_blank(env: &Env, key: &str) -> Option<String> {
    env.var(key).ok().filter(|v| !v.trim().is_empty())
}

/// The value of `key` parsed as `T`, None when it is unset or not a `T`.
fn parsed<T: std::str::FromStr>(env: &Env, key: &str) -> Option<T> {
    env.var(key).ok().and_then(|v| v.parse().ok())
}

// Process-wide (W26): the AppKit callbacks (the focus, the screen of the window) have no context to carry the
// settings. `run` installs them once at start.
static HOST: Slot<HostEnv> = Slot::new("nxkit-host");

/// The installed settings of the host (the defaults in a unit test).
pub fn host() -> &'static HostEnv {
    HOST.get()
}

/// The typed settings of every runtime crate for one run.
#[derive(Clone, Debug)]
pub struct RunSettings {
    /// The host's own keys.
    pub host: HostEnv,
    /// The NVN front end.
    pub nvn: nxkit_nvn::settings::NvnSettings,
    /// The NVN procs of `nxkit-rt` (passed to `nvn::install`).
    pub rt: nxkit_rt::settings::RtSettings,
    /// The audio service.
    pub audio: nxkit_svc_audio::settings::AudioSettings,
    /// The video service.
    pub video: nxkit_svc_video::settings::VideoSettings,
    /// The input service.
    pub input: nxkit_svc_input::settings::InputSettings,
    /// The Unreal take-overs.
    pub ue: nxkit_ue::settings::UeSettings,
    /// The encoder.
    #[cfg(target_os = "macos")]
    pub gpu: nxkit_gpu::GpuEnv,
    /// `NXKIT_PIPELINE_BASES` (read by `nxkit-gpu`), for `GpuSettings`.
    #[cfg(target_os = "macos")]
    pub gpu_bases: bool,
    /// `NXKIT_PIPELINE_SPECIALIZE` (read by `nxkit-gpu`), for `GpuSettings`.
    #[cfg(target_os = "macos")]
    pub gpu_specialize: bool,
    /// The meeting point of the control socket and the encoder for the shader hot swap (W36).
    #[cfg(target_os = "macos")]
    pub shader_hub: nxkit_gpu::ShaderHub,
}

impl RunSettings {
    /// Every crate's settings from the one snapshot.
    pub fn from_env(env: &Env) -> Self {
        #[cfg(target_os = "macos")]
        let gpu = nxkit_gpu::GpuEnv::from_env(env);
        RunSettings {
            #[cfg(target_os = "macos")]
            shader_hub: nxkit_gpu::ShaderHub::new(gpu.diag_dir.join("shaders")),
            host: HostEnv::from_env(env),
            nvn: nxkit_nvn::settings::NvnSettings::from_env(env),
            rt: nxkit_rt::settings::RtSettings::from_env(env),
            audio: nxkit_svc_audio::settings::AudioSettings::from_env(env),
            video: nxkit_svc_video::settings::VideoSettings::from_env(env),
            input: nxkit_svc_input::settings::InputSettings::from_env(env),
            ue: nxkit_ue::settings::UeSettings::from_env(env),
            #[cfg(target_os = "macos")]
            gpu,
            #[cfg(target_os = "macos")]
            gpu_bases: nxkit_gpu::pipeline_bases(env),
            #[cfg(target_os = "macos")]
            gpu_specialize: nxkit_gpu::pipeline_specialize(env),
        }
    }

    /// Installs the settings of the crates that keep them for code with no context. Fails when a crate had them
    /// already or when code read them before: the order of the start is wrong, and the run stops.
    pub fn install(&self) -> Result<(), SlotError> {
        HOST.install(self.host.clone())?;
        nxkit_nvn::settings::install(self.nvn.clone())?;
        nxkit_svc_audio::settings::install(self.audio.clone())?;
        nxkit_svc_video::settings::install(self.video.clone())?;
        nxkit_svc_input::settings::install(self.input.clone())?;
        #[cfg(target_os = "macos")]
        nxkit_gpu::install_env(self.gpu.clone())?;
        Ok(())
    }
}

/// The live keys of every crate (W35), for the board of the run. A crate with live settings adds its section here.
pub fn live_sections() -> Vec<nxkit_abi::live::Section> {
    vec![
        nxkit_nvn::settings::LIVE,
        nxkit_svc_video::settings::LIVE,
        nxkit_svc_pacing::settings::LIVE,
        #[cfg(target_os = "macos")]
        nxkit_gpu::LIVE,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keys_parse_as_the_reads_did() {
        let d = HostEnv::default();
        assert!(d.guest_keep && !d.guest_wire && d.vrr && !d.archive_shards_off && !d.fullscreen);
        assert_eq!((d.shard_lru, d.encode_threads, d.scale), (0, 0, 1.0));
        let s = HostEnv::from_env(&Env::from_pairs([
            ("NXKIT_GUEST_WIRE", "1"),
            ("NXKIT_SET_REFRESH", " 60"),
            ("NXKIT_TIMED_PRESENT", "0"),
            ("NXKIT_ARCHIVE_SHARDS", "off"),
            ("NXKIT_SCALE", "2"),
            ("NXKIT_LANGUAGE", "  "),
            ("NXKIT_PIPELINE_CACHE_DIR", ""),
        ]));
        assert!(s.guest_wire && s.archive_shards_off);
        assert_eq!(s.set_refresh, Some(60));
        assert_eq!(s.timed_present, Some(false));
        assert_eq!(s.scale, 2.0);
        assert_eq!(s.language, None);
        assert_eq!(s.pipeline_cache_dir, None);
        let full =
            |v: &str| HostEnv::from_env(&Env::from_pairs([("NXKIT_FULLSCREEN", v)])).fullscreen;
        assert!(full("1") && full("yes"));
        assert!(!full("0") && !full(" ") && !full(""));
        assert!(d.home_menu && d.home_quit && d.launcher.is_none());
        let h = HostEnv::from_env(&Env::from_pairs([
            ("NXKIT_HOME_MENU", "0"),
            ("NXKIT_LAUNCHER_BUNDLE_ID", " org.example.launcher "),
        ]));
        assert!(!h.home_menu && h.home_quit);
        assert_eq!(h.launcher.as_deref(), Some("org.example.launcher"));
        let op = |v: &str| HostEnv::from_env(&Env::from_pairs([("NXKIT_OP_MODE", v)])).handheld;
        assert_eq!(
            (op("handheld"), op(" docked"), op("console")),
            (Some(true), Some(false), None)
        );
        assert_eq!(d.handheld, None);
        let up = |v: &str| {
            HostEnv::from_env(&Env::from_pairs([("NXKIT_PRESENT_UPSCALE", v)])).present_upscale
        };
        assert_eq!(d.present_upscale, nxkit_metal::PresentUpscale::Off);
        assert_eq!(
            (up("metalfx"), up(" 1"), up("off"), up("0"), up("fsr")),
            (
                nxkit_metal::PresentUpscale::MetalFx,
                nxkit_metal::PresentUpscale::MetalFx,
                nxkit_metal::PresentUpscale::Off,
                nxkit_metal::PresentUpscale::Off,
                nxkit_metal::PresentUpscale::Off
            )
        );
        assert_eq!(
            (d.launcher_pid, d.cursor_hide, d.cursor_hidden),
            (None, 3.0, false)
        );
        let c = HostEnv::from_env(&Env::from_pairs([
            ("NXKIT_LAUNCHER_PID", "4242"),
            ("NXKIT_CURSOR_HIDE", "0"),
            ("NXKIT_CURSOR_HIDDEN", "1"),
        ]));
        assert_eq!(
            (c.launcher_pid, c.cursor_hide, c.cursor_hidden),
            (Some(4242), 0.0, true)
        );
    }

    /// The host reads every launch option, and every value of an option changes what it reads: the table and the
    /// reads cannot drift apart. The audio options are read by the settings of `nxkit-svc-audio`.
    #[test]
    fn every_value_of_a_launch_option_is_read() {
        for o in launch::OPTIONS {
            let values: Vec<String> = match o.kind {
                launch::Kind::Choice(list) => list.iter().map(|(v, _)| v.to_string()).collect(),
                launch::Kind::Toggle { on, off } => vec![on.into(), off.into()],
                launch::Kind::Range { min, max, .. } => vec![min.to_string(), max.to_string()],
            };
            let read = |v: &str| {
                let env = Env::from_pairs([(o.env, v)]);
                (
                    HostEnv::from_env(&env),
                    nxkit_svc_audio::settings::AudioSettings::from_env(&env),
                )
            };
            for (i, a) in values.iter().enumerate() {
                for b in &values[i + 1..] {
                    assert_ne!(read(a), read(b), "{}: {a} and {b} read the same", o.env);
                }
            }
            // the defaults of the console mode, the frame rate, spatial audio and the render size are the title's, which
            // the host has from game.json
            if ![
                launch::OP_MODE,
                launch::FPS_TARGET,
                launch::SPATIAL_AUDIO,
                launch::RENDER_SIZE,
            ]
            .contains(&o.env)
            {
                assert_eq!(
                    read(o.default),
                    Default::default(),
                    "{}: the default is not the unset value",
                    o.env
                );
            }
        }
    }

    #[test]
    fn the_input_maps_are_kept_as_given_unless_blank() {
        let read = |v: &str| {
            let e = HostEnv::from_env(&Env::from_pairs([
                ("NXKIT_PAD_MAP", v),
                ("NXKIT_KEYMAP", v),
            ]));
            (e.pad_map, e.key_map)
        };
        let given = Some("A=B,B=A".to_owned());
        assert_eq!(read("A=B,B=A"), (given.clone(), given));
        assert_eq!(read(" "), (None, None));
    }

    #[test]
    fn the_user_is_a_slot_from_zero() {
        let user = |v: &str| HostEnv::from_env(&Env::from_pairs([("NXKIT_USER", v)])).user;
        assert_eq!((user("1"), user("3"), user(" 8")), (0, 2, 7));
        assert_eq!((user("0"), user("9"), user("x")), (0, 0, 0));
    }

    #[test]
    fn a_dead_zone_is_a_whole_percent_up_to_50() {
        let dz = |v: &str| {
            HostEnv::from_env(&Env::from_pairs([("NXKIT_STICK_DEADZONE_L", v)])).stick_deadzone_l
        };
        assert_eq!((dz("15"), dz("50"), dz("0")), (15, 50, 0));
        assert_eq!((dz("51"), dz("-3"), dz("12.5"), dz("x")), (0, 0, 0, 0));
    }

    #[test]
    fn a_saved_choice_fills_what_the_environment_leaves() {
        let mut env = Env::from_pairs([("NXKIT_FULLSCREEN", "0")]);
        let saved = |k: &str| match k {
            "launcher.NXKIT_OP_MODE" => Some("handheld".to_string()),
            "launcher.NXKIT_FULLSCREEN" => Some("1".to_string()),
            "launcher.NXKIT_HOME_MENU" => Some("off".to_string()),
            _ => None,
        };
        let log = with_saved(&mut env, saved);
        assert_eq!(env.var("NXKIT_OP_MODE").as_deref(), Ok("handheld"));
        assert_eq!(env.var("NXKIT_FULLSCREEN").as_deref(), Ok("0"));
        assert!(env.var("NXKIT_HOME_MENU").is_err());
        assert_eq!(log.len(), 3, "{log:?}");
        let mut off = Env::from_pairs([("NXKIT_SAVED_CHOICES", "0")]);
        with_saved(&mut off, saved);
        assert!(off.var("NXKIT_OP_MODE").is_err());
    }
}
