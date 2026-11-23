//! The settings of the NVN front end from the `NXKIT_*` environment (W27): every key that the crate reads, its default
//! and its parsing. The host builds them once from its snapshot ([`NvnSettings::from_env`]) and installs them
//! ([`install`]) before it builds any part. The code of the crate reads them through [`settings`].
//!
//! The diagnostic keys are live (W35): [`NvnLive`] holds them, the code reads them through [`live`], and the host
//! changes them through [`LIVE`] while the game runs.

use nxkit_abi::env::{Env, Slot, SlotError};
use nxkit_abi::live::{Live, LiveKey, Section};

/// How a render condition reads the counter of an earlier report (`NXKIT_RENDERCOND_STALE`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StaleMode {
    /// `0`: no counter of an earlier report.
    Off,
    /// `addr`: only the one at the same address.
    SameAddress,
    /// The default: the one at the same address, else the one at the same place of an earlier frame.
    #[default]
    SamePlace,
}

/// The settings of the NVN front end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NvnSettings {
    /// `NXKIT_KNOB` names `nopassmerge`: the pass optimiser merges no passes (unless the host sets the options).
    pub knob_no_pass_merge: bool,
    /// `NXKIT_KNOB` names `noloadstore`: the pass optimiser keeps every store (unless the host sets the options).
    pub knob_no_loadstore: bool,
    /// `NXKIT_COUNTERS` (on unless `0`): the GPU counters of the reports.
    pub counters: bool,
    /// `NXKIT_RENDERCOND_STALE`.
    pub rendercond_stale: StaleMode,
    /// Whether `NXKIT_RENDERCOND_STALE` is set. Without it the host takes `rendercond_stale` of `game.json`
    /// ([`crate::rendercond::set_title_stale`]).
    pub rendercond_stale_set: bool,
    /// `NXKIT_RENDERCOND=3d`: a render condition leaves out draws and clears only.
    pub rendercond_3d: bool,
    /// `NXKIT_VERIFY_CACHE=1`: the decoder checks each cached summary against a fresh decode.
    pub verify_cache: bool,
}

/// The live settings of the NVN front end (W35).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NvnLive {
    /// `NXKIT_DIAG`: the diagnostic categories that are on, comma separated. `None` means `all`.
    pub diag: Option<Vec<String>>,
    /// `NXKIT_PACE_DIAG=1`: the spread of the intervals between shown frames.
    pub pace_diag: bool,
    /// `NXKIT_WATCH_ADDR` (hex, `0x` optional): the decoder prints what the record word at that address resolves to.
    pub watch_addr: Option<u64>,
}

impl Default for NvnLive {
    fn default() -> Self {
        NvnLive::from_env(&Env::default())
    }
}

impl NvnLive {
    /// The live settings from a snapshot.
    pub fn from_env(env: &Env) -> Self {
        let diag = {
            let raw = env.var("NXKIT_DIAG").unwrap_or_default();
            let list: Vec<String> = raw
                .split(',')
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .map(String::from)
                .collect();
            (!list.iter().any(|c| c == "all")).then_some(list)
        };
        NvnLive {
            diag,
            pace_diag: env.var("NXKIT_PACE_DIAG").is_ok_and(|v| v == "1"),
            watch_addr: env
                .var("NXKIT_WATCH_ADDR")
                .ok()
                .and_then(|v| u64::from_str_radix(v.trim().trim_start_matches("0x"), 16).ok()),
        }
    }
}

impl Default for NvnSettings {
    fn default() -> Self {
        NvnSettings::from_env(&Env::default())
    }
}

impl NvnSettings {
    /// The settings from the snapshot of the host.
    pub fn from_env(env: &Env) -> Self {
        let knob = env.var("NXKIT_KNOB").unwrap_or_default();
        let has = |name: &str| knob.split(',').any(|k| k.trim() == name);
        NvnSettings {
            knob_no_pass_merge: has("nopassmerge"),
            knob_no_loadstore: has("noloadstore"),
            counters: env.var("NXKIT_COUNTERS").map_or(true, |v| v != "0"),
            rendercond_stale: match env.var("NXKIT_RENDERCOND_STALE").as_deref() {
                Ok("0") => StaleMode::Off,
                Ok("addr") => StaleMode::SameAddress,
                _ => StaleMode::SamePlace,
            },
            rendercond_stale_set: env.var("NXKIT_RENDERCOND_STALE").is_ok(),
            rendercond_3d: env.var("NXKIT_RENDERCOND").is_ok_and(|v| v == "3d"),
            verify_cache: env.var("NXKIT_VERIFY_CACHE").is_ok_and(|v| v == "1"),
        }
    }
}

// Process-wide (W26): the recorder of the guest's command buffers and the diagnostics of the encoder read these
// switches with no context to carry them. The host installs them once at start.
static SETTINGS: Slot<NvnSettings> = Slot::new("nxkit-nvn");

/// Installs the settings. The host calls it once, before it builds any part.
pub fn install(settings: NvnSettings) -> Result<(), SlotError> {
    SETTINGS.install(settings)
}

/// The installed settings (the defaults in a unit test).
pub fn settings() -> &'static NvnSettings {
    SETTINGS.get()
}

// Process-wide (W33 (2)): the live part of the settings, for the same code with no context.
static LIVE_CELL: Live<NvnLive> = Live::new();

/// The live settings now (the defaults in a unit test).
pub fn live() -> &'static NvnLive {
    LIVE_CELL.get()
}

/// The live keys of the crate, for the board of the host.
pub const LIVE: Section = Section {
    owner: "nxkit-nvn",
    keys: &[
        LiveKey {
            name: "NXKIT_DIAG",
            help: "the diagnostic categories, comma separated (all for every one)",
        },
        LiveKey {
            name: "NXKIT_PACE_DIAG",
            help: "1: the pace lines of the shown frames",
        },
        LiveKey {
            name: "NXKIT_WATCH_ADDR",
            help: "a guest address (hex): what the record word there resolves to",
        },
    ],
    apply: |env| LIVE_CELL.set(NvnLive::from_env(env)),
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keys_parse_as_the_reads_did() {
        let d = NvnSettings::default();
        assert_eq!(NvnLive::default().diag, Some(Vec::new()));
        assert!(d.counters && !NvnLive::default().pace_diag && !d.rendercond_3d);
        assert_eq!(d.rendercond_stale, StaleMode::SamePlace);
        let env = Env::from_pairs([
            ("NXKIT_DIAG", "health, stats"),
            ("NXKIT_KNOB", "x,nopassmerge"),
            ("NXKIT_COUNTERS", "0"),
            ("NXKIT_RENDERCOND_STALE", "addr"),
            ("NXKIT_WATCH_ADDR", "0x1f00"),
        ]);
        let s = NvnSettings::from_env(&env);
        assert_eq!(
            NvnLive::from_env(&env).diag,
            Some(vec!["health".to_string(), "stats".to_string()])
        );
        assert!(s.knob_no_pass_merge && !s.knob_no_loadstore && !s.counters);
        assert_eq!(s.rendercond_stale, StaleMode::SameAddress);
        assert_eq!(NvnLive::from_env(&env).watch_addr, Some(0x1f00));
        let all = NvnLive::from_env(&Env::from_pairs([("NXKIT_DIAG", "stats,all")]));
        assert_eq!(all.diag, None);
    }
}
