//! The live settings of the encoder (W35): the keys of the diagnostics that the host changes while the game runs.
//! They live apart from `crate::settings`, which re-exports them.

use nxkit_abi::env::Env;
use nxkit_abi::live::{Live, LiveKey, Section};

/// The live settings of the encoder (W35): the keys of the diagnostics that the host changes while the game runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuLive {
    /// `NXKIT_KNOB`: the knobs of the experiments, comma separated.
    pub knob: String,
    /// `NXKIT_PAINT_FS`: the fragment program keys that the `paint` knobs paint.
    pub paint_fs: Vec<String>,
    /// `NXKIT_DIAG_FS`: the fragment program keys that the `draw` line and `nanscan` look at.
    pub diag_fs: Vec<String>,
    /// `NXKIT_DIAG_TEX`: the texture filter of the diagnostics.
    pub diag_tex: Option<TexFilter>,
    /// `NXKIT_WATCH_TEX`: the texture ids that the diagnostics follow.
    pub watch_tex: Vec<u32>,
    /// `NXKIT_GPU_LABELS=1`: the passes get labels for the GPU tools.
    pub gpu_labels: bool,
    /// `NXKIT_SKIP_PROGRAMS`: the program keys whose draws are left out (comma or line separated).
    pub skip_programs: Vec<String>,
    /// `NXKIT_DIAG_WATCH=0xADDR:LEN,..`: the guest ranges `(address, len)` that the diagnostics watch (len at most 512).
    pub diag_watch: Vec<(u64, usize)>,
    /// `NXKIT_PASS_LIST` is set: the pass diagnostic lists every pass.
    pub pass_list: bool,
    /// `NXKIT_BARPASS=<pass index>`: the pass whose barrier plan is traced.
    pub bar_pass: Option<usize>,
    /// `NXKIT_TRACE_FS=<fragment program key>[:slot,slot,..]`: the draws that are traced.
    pub trace_fs: Option<FsTrace>,
    /// `NXKIT_TRACE_HANDLES=<handle,..>`: the bindless handles whose resolve is traced.
    pub trace_handles: Vec<u64>,
}

/// The draws of one fragment program that `NXKIT_TRACE_FS` traces, and the texture slots that it prints.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FsTrace {
    /// The shader key of the fragment program.
    pub program: String,
    /// The fragment texture slots. Empty means every bound slot.
    pub slots: Vec<u32>,
}

impl FsTrace {
    /// Parses `KEY` or `KEY:slot,slot,..`. An empty key gives none. A slot that is not a number is left out.
    pub fn parse(v: &str) -> Option<Self> {
        let (key, slots) = v.split_once(':').unwrap_or((v, ""));
        let program = key.trim();
        if program.is_empty() {
            return None;
        }
        Some(FsTrace {
            program: program.to_owned(),
            slots: slots
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect(),
        })
    }

    /// Whether the trace prints fragment texture slot `slot`.
    pub fn wants(&self, slot: u32) -> bool {
        self.slots.is_empty() || self.slots.contains(&slot)
    }
}

/// The handles of a list, comma separated, in decimal or as `0x` hex. A part that is not a number is left out.
pub(crate) fn handle_list(v: &str) -> Vec<u64> {
    v.split(',')
        .filter_map(|s| {
            let s = s.trim();
            match s.strip_prefix("0x") {
                Some(h) => u64::from_str_radix(h, 16).ok(),
                None => s.parse().ok(),
            }
        })
        .collect()
}

impl Default for GpuLive {
    fn default() -> Self {
        GpuLive::from_env(&Env::default())
    }
}

impl GpuLive {
    /// The live settings from a snapshot.
    pub fn from_env(env: &Env) -> Self {
        let one = |k: &str| env.var(k).is_ok_and(|v| v == "1");
        let keys = |k: &str| -> Vec<String> {
            env.var(k)
                .map(|v| {
                    v.split(',')
                        .map(|s| s.trim().to_owned())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default()
        };
        GpuLive {
            knob: env.var("NXKIT_KNOB").unwrap_or_default(),
            paint_fs: keys("NXKIT_PAINT_FS"),
            diag_fs: keys("NXKIT_DIAG_FS"),
            diag_tex: env.var("NXKIT_DIAG_TEX").ok().map(|v| TexFilter::parse(&v)),
            watch_tex: env
                .var("NXKIT_WATCH_TEX")
                .map(|v| v.split(',').filter_map(|s| s.trim().parse().ok()).collect())
                .unwrap_or_default(),
            gpu_labels: one("NXKIT_GPU_LABELS"),
            skip_programs: env
                .var("NXKIT_SKIP_PROGRAMS")
                .map(|v| skip_keys(&v))
                .unwrap_or_default(),
            diag_watch: env
                .var("NXKIT_DIAG_WATCH")
                .map(|v| watch_ranges(&v))
                .unwrap_or_default(),
            pass_list: env.var_os("NXKIT_PASS_LIST").is_some(),
            bar_pass: env
                .var("NXKIT_BARPASS")
                .ok()
                .and_then(|v| v.trim().parse().ok()),
            trace_fs: env
                .var("NXKIT_TRACE_FS")
                .ok()
                .and_then(|v| FsTrace::parse(&v)),
            trace_handles: env
                .var("NXKIT_TRACE_HANDLES")
                .map(|v| handle_list(&v))
                .unwrap_or_default(),
        }
    }
}

/// The texture filter of `NXKIT_DIAG_TEX`: a format name (as `Debug` prints it), a size `WxH` or `WxHxL` (layers),
/// or both, separated by `/` (`R16Unorm/256x256`). Every part that is given has to match.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TexFilter {
    /// The format name.
    pub format: Option<String>,
    /// The size.
    pub size: Option<[u32; 2]>,
    /// The layers.
    pub layers: Option<u32>,
}

impl TexFilter {
    fn parse(v: &str) -> Self {
        let mut f = TexFilter::default();
        for part in v.split('/').map(str::trim).filter(|p| !p.is_empty()) {
            let nums: Vec<u32> = part.split('x').filter_map(|n| n.parse().ok()).collect();
            match nums.as_slice() {
                [w, h] => f.size = Some([*w, *h]),
                [w, h, l] => {
                    f.size = Some([*w, *h]);
                    f.layers = Some(*l);
                }
                _ => f.format = Some(part.to_owned()),
            }
        }
        f
    }
}

/// The program keys of a skip list, comma or line separated (`NXKIT_SKIP_PROGRAMS`, `NXKIT_SKIP_FILE`).
pub(crate) fn skip_keys(v: &str) -> Vec<String> {
    v.split([',', '\n'])
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .collect()
}

fn watch_ranges(v: &str) -> Vec<(u64, usize)> {
    v.split(',')
        .filter_map(|s| {
            let (a, l) = s.trim().split_once(':')?;
            let a = u64::from_str_radix(a.trim().trim_start_matches("0x"), 16).ok()?;
            let l = l.trim();
            let l = match l.strip_prefix("0x") {
                Some(h) => usize::from_str_radix(h, 16).ok()?,
                None => l.parse().ok()?,
            };
            Some((a, l.min(512)))
        })
        .collect()
}

// Process-wide (W33 (2)): the live part, for the same code with no context.
static LIVE_CELL: Live<GpuLive> = Live::new();

/// The live settings now (the defaults in a unit test).
pub(crate) fn live() -> &'static GpuLive {
    LIVE_CELL.get()
}

/// The live keys of the encoder, for the board of the host.
pub const LIVE: Section = Section {
    owner: "nxkit-gpu",
    keys: &[
        LiveKey {
            name: "NXKIT_KNOB",
            help: "the knobs of the experiments, comma separated (NXKIT_KNOB_FILE wins)",
        },
        LiveKey {
            name: "NXKIT_PAINT_FS",
            help: "the fragment program keys that the paint knobs paint",
        },
        LiveKey {
            name: "NXKIT_DIAG_FS",
            help: "the fragment program keys that the draw line and nanscan look at",
        },
        LiveKey {
            name: "NXKIT_DIAG_TEX",
            help: "the texture filter of the diagnostics: a format, WxH or WxHxL, or both with /",
        },
        LiveKey {
            name: "NXKIT_WATCH_TEX",
            help: "the texture ids whose passes and copies are printed, comma separated",
        },
        LiveKey {
            name: "NXKIT_GPU_LABELS",
            help: "1: the passes get labels for Instruments",
        },
        LiveKey {
            name: "NXKIT_SKIP_PROGRAMS",
            help: "the program keys whose draws are left out, comma separated",
        },
        LiveKey {
            name: "NXKIT_DIAG_WATCH",
            help: "the guest ranges 0xADDR:LEN that the diagnostics watch, comma separated",
        },
        LiveKey {
            name: "NXKIT_PASS_LIST",
            help: "set: the pass diagnostic lists every pass",
        },
        LiveKey {
            name: "NXKIT_BARPASS",
            help: "the index of a pass whose barrier plan is traced (every 60th frame)",
        },
        LiveKey {
            name: "NXKIT_TRACE_FS",
            help: "FRAGMENT_KEY[:slot,slot,..]: traces every 300th draw of the program and its textures",
        },
        LiveKey {
            name: "NXKIT_TRACE_HANDLES",
            help: "the bindless handles whose resolve is traced, comma separated",
        },
    ],
    apply: |env| LIVE_CELL.set(GpuLive::from_env(env)),
};
