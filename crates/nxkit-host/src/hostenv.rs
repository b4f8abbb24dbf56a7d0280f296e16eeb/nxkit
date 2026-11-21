//! The settings of the run from the `NXKIT_*` environment (W27).
//!
//! [`HostEnv`] has every key that the host itself reads, its default and its parsing. [`RunSettings`] holds the typed
//! settings of every runtime crate, each built by the crate's own `from_env` from the one snapshot that `run` takes.
//! [`RunSettings::install`] gives the crates that keep their settings for code with no context (the NVN front end, the
//! encoder, the services) their settings, and the host passes the rest explicitly when it builds the parts.

use std::path::PathBuf;

use nxkit_abi::env::{Env, Slot, SlotError};

/// The settings of the host itself.
#[derive(Clone, Debug, PartialEq)]
pub struct HostEnv {
    /// `NXKIT_MEM_RESERVE` as given: the heap region in place of the title's (a size as `parse_size` reads it).
    pub mem_reserve: Option<String>,
    /// `NXKIT_MEM_ALIAS` as given: the alias region in place of the title's.
    pub mem_alias: Option<String>,
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
    /// `NXKIT_PAUSE_UNFOCUSED=1`: the game pauses when the window loses the focus.
    pub pause_unfocused: bool,
    /// `NXKIT_SCREEN`: the screen of the window, by index or a part of its name.
    pub screen: Option<String>,
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
                .var("NXKIT_LANGUAGE")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            mem_report: env
                .var("NXKIT_MEM_REPORT")
                .ok()
                .and_then(|v| v.trim().parse().ok()),
            crash_report_off: env.var("NXKIT_CRASH_REPORT").is_ok_and(|v| v == "0"),
            pause_unfocused: env.var("NXKIT_PAUSE_UNFOCUSED").is_ok_and(|v| v == "1"),
            screen: env.var("NXKIT_SCREEN").ok(),
        }
    }
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
    /// The pacing service.
    pub pacing: nxkit_svc_pacing::settings::PacingSettings,
    /// The encoder.
    #[cfg(target_os = "macos")]
    pub gpu: nxkit_gpu::GpuEnv,
    /// `NXKIT_PIPELINE_BASES` (read by `nxkit-gpu`), for `GpuSettings`.
    #[cfg(target_os = "macos")]
    pub gpu_bases: bool,
    /// `NXKIT_PIPELINE_SPECIALIZE` (read by `nxkit-gpu`), for `GpuSettings`.
    #[cfg(target_os = "macos")]
    pub gpu_specialize: bool,
}

impl RunSettings {
    /// Every crate's settings from the one snapshot.
    pub fn from_env(env: &Env) -> Self {
        RunSettings {
            host: HostEnv::from_env(env),
            nvn: nxkit_nvn::settings::NvnSettings::from_env(env),
            rt: nxkit_rt::settings::RtSettings::from_env(env),
            audio: nxkit_svc_audio::settings::AudioSettings::from_env(env),
            video: nxkit_svc_video::settings::VideoSettings::from_env(env),
            input: nxkit_svc_input::settings::InputSettings::from_env(env),
            pacing: nxkit_svc_pacing::settings::PacingSettings::from_env(env),
            #[cfg(target_os = "macos")]
            gpu: nxkit_gpu::GpuEnv::from_env(env),
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
        nxkit_svc_pacing::settings::install(self.pacing.clone())?;
        #[cfg(target_os = "macos")]
        nxkit_gpu::install_env(self.gpu.clone())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keys_parse_as_the_reads_did() {
        let d = HostEnv::default();
        assert!(d.guest_keep && !d.guest_wire && d.vrr && !d.archive_shards_off);
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
    }
}
