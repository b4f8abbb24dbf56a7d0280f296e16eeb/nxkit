//! What the host tells the encoder: [`GpuSettings`] from the settings of the title, and [`GpuEnv`] from the `NXKIT_*`
//! snapshot of the host (W27).

use std::path::PathBuf;

use nxkit_abi::env::{Env, Slot, SlotError};
use nxkit_abi::live::{Live, LiveKey, Section};

use nxkit_metal::GobOrder;

/// The settings the encoder and the present path use.
#[derive(Clone, Debug)]
pub struct GpuSettings {
    /// The folder with the translated shaders (`<run>/shaders`).
    pub shader_dir: PathBuf,
    /// The Metal 4 pipeline archive to load, when the title uses one.
    pub archive: Option<PathBuf>,
    /// The folder of the archive shards (`<run>/pipelines`). Its index wins over `archive`, which Metal reads whole
    /// into memory.
    pub archive_shards: Option<PathBuf>,
    /// How many shards stay open at once. Zero picks the default of the Metal backend (32).
    pub open_shards: usize,
    /// Collect the pipelines that get built, so an archive can be written.
    pub collect_archive: bool,
    /// The threads that encode passes. Zero picks a number from the cores.
    pub threads: usize,
    /// The factor of the render size over the size the game asks for. 2.0 renders a 1080p game at 4K.
    pub scale: f32,
    /// Encode the frames on a thread of their own (`game.json` key `enc_thread`). The environment variable
    /// `NXKIT_ENC_THREAD=1` turns it on too.
    pub enc_thread: bool,
    /// The CPU runs one frame ahead of the GPU (`game.json` key `overlap`). `NXKIT_OVERLAP=1` turns it on too.
    pub overlap: bool,
    /// The order inside a GOB of the guest textures: [`GobOrder::Switch2`] for a game of the Switch 2.
    pub gob_order: GobOrder,
    /// A writable folder of the title for the runtime pipeline cache (see `pipelines/runtime.rs`), or none.
    pub runtime_cache: Option<PathBuf>,
    /// A number that changes with the app build: a runtime cache of another one is dropped.
    pub build_id: u64,
    /// The pipeline list that the build shipped in the app (`nxkit_pipestate::list::SHIPPED_LIST`), or none.
    pub shipped_list: Option<PathBuf>,
    /// The pipeline cache compiles the unspecialized pipeline of a base that the archive lacks and specializes it per
    /// colour state (`ArchiveConfig::compile_bases`): off unless `NXKIT_PIPELINE_BASES=1`. In Ocarina of Time every
    /// base was as rare as its pipelines (the vertex layout is part of it), an unspecialized compile took 0.1 to 10 s
    /// where the full compile took 1 to 6 ms, and text of the title screen went missing.
    pub compile_bases: bool,
    /// The pipeline cache specializes the unspecialized pipelines of the archive (`ArchiveConfig::exact_only` off):
    /// on unless `NXKIT_PIPELINE_SPECIALIZE=0`, which looks up and compiles exact pipelines only.
    pub specialize: bool,
    /// What the start shows while it makes the pipelines of the runtime cache (the host's loading screen).
    pub prewarm_screen: crate::prewarm_screen::ScreenHook,
    /// The hooks of the `frame` and `gateseq` traces for the batches, when one of them is on.
    pub batch_hooks: Option<nxkit_nvn::diag::BatchHooks>,
    /// The minimum stay of the presents that the host's refresh plan sets (timed presents).
    pub timed_present: std::sync::Arc<nxkit_metal::TimedPresent>,
    /// What the screen showed of the presents. The backend writes it, the NVN present reads it (W26).
    pub screen: std::sync::Arc<nxkit_nvn::screen::Screen>,
    /// How the present scales a picture that is smaller than the drawable (R70).
    pub present_upscale: nxkit_metal::PresentUpscale,
    /// The draws that the temporal upscale replaces (`temporal_upscale` of `game.json`, R72), or none.
    /// `NXKIT_TEMPORAL_UPSCALE=0` turns it off.
    pub temporal_upscale: Option<TemporalUpscale>,
    /// The meeting point with the control socket for the shader hot swap (W36), or none.
    pub shader_hub: Option<crate::ShaderHub>,
}

/// The draws of a game that the temporal upscale replaces and how it reads their inputs (R72, `tupscale`). The
/// fragment programs are named by their shader keys and the textures by their fragment texture slots.
#[derive(Clone, Debug, PartialEq)]
pub struct TemporalUpscale {
    /// The upscale draw: the first draw of its render pass. `slot` is the jittered picture of the input size. The
    /// scaler's picture goes to colour attachment 0 of its pass instead.
    pub upscale: DrawTexture,
    /// The draw whose bindings give the depth, the velocity and the view uniform buffer: the last one before the
    /// upscale draw.
    pub source: TemporalSource,
    /// The draws that are replaced by a copy of the texture at `slot` into colour attachment 0 of their pass. Each is
    /// the first draw of its pass.
    pub passthrough: Vec<DrawTexture>,
    /// Where the view uniform buffer keeps its values.
    pub view: ViewLayout,
    /// How a texel of the velocity texture turns into a velocity.
    pub velocity: VelocityCode,
    /// True when the depth is 1 at the near plane.
    pub depth_reversed: bool,
}

/// A draw by its fragment program and one of its fragment texture slots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrawTexture {
    /// The shader key of the fragment program.
    pub fragment: String,
    /// The fragment texture slot.
    pub slot: u32,
}

/// The draw that gives the inputs of the scaler besides the picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemporalSource {
    /// The shader key of the fragment program.
    pub fragment: String,
    /// The fragment texture slot of the scene depth.
    pub depth_slot: u32,
    /// The fragment texture slot of the velocity.
    pub velocity_slot: u32,
    /// The uniform buffer that holds the view values.
    pub view: UboSlot,
}

/// A uniform buffer slot of one stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UboSlot {
    /// The stage.
    pub stage: crate::detail::Stage,
    /// The slot.
    pub slot: u32,
}

/// The byte offsets of the view values in their uniform buffer. The floats are little-endian.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewLayout {
    /// The matrix from this frame's clip space to the previous frame's, 16 floats by rows, for a row vector.
    pub clip_to_prev_clip: u32,
    /// The jitter of this frame in clip units (x, y), followed by the previous one.
    pub jitter: u32,
    /// The view size in pixels (x, y), followed by its inverse.
    pub view_size: u32,
}

/// The velocity `V = raw * scale + bias` of a texel `raw` of the velocity texture, in clip units, current minus
/// previous position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VelocityCode {
    /// The factor per channel.
    pub scale: [f32; 2],
    /// The offset per channel.
    pub bias: [f32; 2],
    /// True when `V.y` points up, as clip space does.
    pub y_up: bool,
    /// True when a texel of exactly 0 in both channels has no velocity written, and the camera motion from the
    /// depth and the matrix stands in.
    pub zero_means_camera: bool,
}

/// [`GpuSettings::compile_bases`] from the settings of the host (`NXKIT_PIPELINE_BASES`).
pub fn pipeline_bases(env: &nxkit_abi::env::Env) -> bool {
    env.var("NXKIT_PIPELINE_BASES").is_ok_and(|v| v == "1")
}

/// [`GpuSettings::specialize`] from the settings of the host (`NXKIT_PIPELINE_SPECIALIZE`).
pub fn pipeline_specialize(env: &nxkit_abi::env::Env) -> bool {
    env.var("NXKIT_PIPELINE_SPECIALIZE")
        .map_or(true, |v| v != "0")
}

impl Default for GpuSettings {
    fn default() -> Self {
        Self {
            shader_dir: PathBuf::from("shaders"),
            archive: None,
            archive_shards: None,
            open_shards: 0,
            collect_archive: false,
            threads: 0,
            scale: 2.0,
            enc_thread: false,
            overlap: false,
            gob_order: GobOrder::Tegra,
            runtime_cache: None,
            build_id: 0,
            shipped_list: None,
            compile_bases: pipeline_bases(&Env::default()),
            specialize: pipeline_specialize(&Env::default()),
            prewarm_screen: crate::prewarm_screen::ScreenHook::default(),
            batch_hooks: None,
            timed_present: std::sync::Arc::default(),
            screen: std::sync::Arc::default(),
            present_upscale: nxkit_metal::PresentUpscale::Off,
            temporal_upscale: None,
            shader_hub: None,
        }
    }
}

/// One native network's switches (`NXKIT_<NAME>`, `_COMPARE`, `_ANE`, `_ANE_CACHE`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeNet {
    /// `NXKIT_<NAME>` (on unless `0`): the network runs natively.
    pub enabled: bool,
    /// `NXKIT_<NAME>_COMPARE=1`: the game's chain runs next to the plan on a sampled frame.
    pub compare: bool,
    /// `NXKIT_<NAME>_ANE` (on unless `0`): a network that can run on the Neural Engine does.
    pub ane: bool,
    /// `NXKIT_<NAME>_ANE_CACHE`: the folder of the compiled Core ML model.
    pub ane_cache: Option<String>,
}

impl Default for NativeNet {
    fn default() -> Self {
        NativeNet::from_env(&Env::default(), "")
    }
}

impl NativeNet {
    /// The names of the networks that have switches.
    pub const NAMES: [&'static str; 2] = ["ndve", "ndsr"];

    fn from_env(env: &Env, net: &str) -> Self {
        let name = |suffix: &str| format!("NXKIT_{}{suffix}", net.to_ascii_uppercase());
        NativeNet {
            enabled: env.var(name("")).map_or(true, |v| v != "0"),
            compare: env.var(name("_COMPARE")).is_ok_and(|v| v == "1"),
            ane: env.var(name("_ANE")).map_or(true, |v| v != "0"),
            ane_cache: env.var(name("_ANE_CACHE")).ok(),
        }
    }
}

/// The settings of the encoder from the `NXKIT_*` environment (W27): every key that the crate reads besides
/// [`GpuSettings`], its default and its parsing. The host builds them once ([`GpuEnv::from_env`]) and installs them
/// ([`install`]) before it builds the backend. The code of the crate reads them through `env()`.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuEnv {
    /// `NXKIT_COPYBACK` (on unless `0`): GPU-written textures in CPU pools are copied back.
    pub copyback: bool,
    /// `NXKIT_COPYBACK_MAX_MB` (128): the storage MiB that one frame may copy back.
    pub copyback_max_mb: u64,
    /// `NXKIT_PRIVATE` as given (`track`, `1`). The mode is read from it in `private`.
    pub private: Option<String>,
    /// `NXKIT_PRIVATE_AHEAD` (1): the frames the mirror may run ahead.
    pub private_ahead: u64,
    /// `NXKIT_PRIVATE_LATE` (on unless `0`): results one frame late.
    pub private_late: bool,
    /// `NXKIT_PRIVATE_READOPEN` (on unless `0`): reads without a trap.
    pub private_read_open: bool,
    /// `NXKIT_OVERLAP=1`: the CPU runs one frame ahead of the GPU.
    pub overlap: bool,
    /// `NXKIT_DIAG_DIR` (`/tmp/fmap`): where the diagnostics write files.
    pub diag_dir: PathBuf,
    /// `NXKIT_KNOB_FILE`: a file with more knobs, read every frame.
    pub knob_file: Option<PathBuf>,
    /// `NXKIT_SKIP_FILE`: a file with more programs to leave out.
    pub skip_file: Option<PathBuf>,
    /// `NXKIT_MEM_REPORT` is a number above 0: the memory report.
    pub mem_report: bool,
    /// `NXKIT_WALK_CACHE=1`: the walk caches its summaries.
    pub walk_cache: bool,
    /// `NXKIT_VERIFY_CACHE=1`: the walk checks each cached summary.
    pub verify_cache: bool,
    /// `NXKIT_TEX_EVICT` as given: the frames after which an unused texture is dropped.
    pub tex_evict: Option<String>,
    /// `NXKIT_TEX_SUPERSEDE` (on unless `0`).
    pub tex_supersede: bool,
    /// `NXKIT_TEX_INPLACE` (on unless `0`): textures over guest memory.
    pub tex_inplace: bool,
    /// The switches of the native networks, by name.
    pub native: Vec<(&'static str, NativeNet)>,
    /// `NXKIT_NATIVE_CACHE` (4): the plans that the native cache keeps.
    pub native_cache: usize,
    /// `NXKIT_STREAM_COMMIT` (on unless `0`): the first command buffers of a frame commit early.
    pub stream_commit: bool,
    /// `NXKIT_PIPELINE_THREADS` (above 0): the threads that compile pipelines, else from the cores.
    pub pipeline_threads: Option<usize>,
    /// `NXKIT_PIPELINE_QOS=utility`: the compile threads run at the utility class.
    pub pipeline_qos_utility: bool,
    /// `NXKIT_PIPELINE_ASYNC` (on unless `0`): pipelines compile in the background.
    pub pipeline_async: bool,
    /// `NXKIT_PULL=1`: vertex pulling.
    pub pull: bool,
    /// `NXKIT_PIPELINE_CACHE` (on unless `0`): the runtime pipeline cache.
    pub pipeline_cache: bool,
    /// `NXKIT_PIPELINE_CACHE_MB` (512).
    pub pipeline_cache_mb: u64,
    /// `NXKIT_PIPELINE_CACHE_FLUSH` (30 s).
    pub pipeline_cache_flush: u64,
    /// `NXKIT_PIPELINE_CACHE_ROTATE` (1000).
    pub pipeline_cache_rotate: u64,
    /// `NXKIT_PIPELINE_PREWARM` (on unless `0`).
    pub pipeline_prewarm: bool,
    /// `NXKIT_PIPELINE_PREWARM_WAIT` (20 s).
    pub pipeline_prewarm_wait: u64,
    /// `NXKIT_SHARD_TRIM` (on unless `0`): the open shards close after the start.
    pub shard_trim: bool,
    /// `NXKIT_PIPELINE_RECORD` (on unless `0`): the runtime cache records the pipelines.
    pub pipeline_record: bool,
    /// `NXKIT_ENC_THREAD=1`: the encoder thread, besides the `game.json` key.
    pub enc_thread: bool,
    /// `NXKIT_TEMPORAL_UPSCALE` (on unless `0`): the temporal upscale of `game.json` replaces the game's draws (R72).
    pub temporal_upscale: bool,
}

impl Default for GpuEnv {
    fn default() -> Self {
        GpuEnv::from_env(&Env::default())
    }
}

impl GpuEnv {
    /// The settings from the snapshot of the host.
    pub fn from_env(env: &Env) -> Self {
        let on = |k: &str| env.var(k).map_or(true, |v| v != "0");
        let one = |k: &str| env.var(k).is_ok_and(|v| v == "1");
        let not_zero = |k: &str| !env.var(k).is_ok_and(|v| v.trim() == "0");
        let num = |k: &str, d: u64| {
            env.var(k)
                .ok()
                .and_then(|v| v.parse().ok())
                .filter(|n| *n > 0)
                .unwrap_or(d)
        };
        GpuEnv {
            copyback: env.var("NXKIT_COPYBACK").map_or(true, |v| v.trim() != "0"),
            copyback_max_mb: env
                .var("NXKIT_COPYBACK_MAX_MB")
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(128),
            private: env.var("NXKIT_PRIVATE").ok(),
            private_ahead: env
                .var("NXKIT_PRIVATE_AHEAD")
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(1),
            private_late: not_zero("NXKIT_PRIVATE_LATE"),
            private_read_open: not_zero("NXKIT_PRIVATE_READOPEN"),
            overlap: one("NXKIT_OVERLAP"),
            diag_dir: env
                .var_os("NXKIT_DIAG_DIR")
                .map_or_else(|| PathBuf::from("/tmp/fmap"), PathBuf::from),
            knob_file: env.var_os("NXKIT_KNOB_FILE").map(PathBuf::from),
            skip_file: env.var_os("NXKIT_SKIP_FILE").map(Into::into),
            mem_report: env
                .var("NXKIT_MEM_REPORT")
                .ok()
                .and_then(|v| v.trim().parse::<f64>().ok())
                .is_some_and(|s| s > 0.0),
            walk_cache: one("NXKIT_WALK_CACHE"),
            verify_cache: one("NXKIT_VERIFY_CACHE"),
            tex_evict: env.var("NXKIT_TEX_EVICT").ok(),
            tex_supersede: not_zero("NXKIT_TEX_SUPERSEDE"),
            tex_inplace: on("NXKIT_TEX_INPLACE"),
            native: NativeNet::NAMES
                .iter()
                .map(|n| (*n, NativeNet::from_env(env, n)))
                .collect(),
            native_cache: env
                .var("NXKIT_NATIVE_CACHE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(4),
            stream_commit: !matches!(env.var("NXKIT_STREAM_COMMIT").as_deref(), Ok("0")),
            pipeline_threads: env
                .var("NXKIT_PIPELINE_THREADS")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|n| *n > 0),
            pipeline_qos_utility: env.var("NXKIT_PIPELINE_QOS").is_ok_and(|v| v == "utility"),
            pipeline_async: on("NXKIT_PIPELINE_ASYNC"),
            pull: env.var("NXKIT_PULL").ok().as_deref() == Some("1"),
            pipeline_cache: on("NXKIT_PIPELINE_CACHE"),
            pipeline_cache_mb: num("NXKIT_PIPELINE_CACHE_MB", 512),
            pipeline_cache_flush: num("NXKIT_PIPELINE_CACHE_FLUSH", 30),
            pipeline_cache_rotate: num("NXKIT_PIPELINE_CACHE_ROTATE", 1000),
            pipeline_prewarm: on("NXKIT_PIPELINE_PREWARM"),
            pipeline_prewarm_wait: num("NXKIT_PIPELINE_PREWARM_WAIT", 20),
            shard_trim: on("NXKIT_SHARD_TRIM"),
            pipeline_record: on("NXKIT_PIPELINE_RECORD"),
            enc_thread: one("NXKIT_ENC_THREAD"),
            temporal_upscale: on("NXKIT_TEMPORAL_UPSCALE"),
        }
    }

    /// The switches of the native network `net` (the defaults for a network without switches).
    pub fn native(&self, net: &str) -> NativeNet {
        self.native
            .iter()
            .find(|(n, _)| *n == net)
            .map_or_else(NativeNet::default, |(_, s)| s.clone())
    }
}

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
    ],
    apply: |env| LIVE_CELL.set(GpuLive::from_env(env)),
};

// Process-wide (W26): the switches deep in the encoder (the copy-back, the mirror, the diagnostics) have no context to
// carry the settings. The host installs them once at start.
static ENV: Slot<GpuEnv> = Slot::new("nxkit-gpu");

/// Installs the settings. The host calls it once, before it builds the backend.
pub fn install(env: GpuEnv) -> Result<(), SlotError> {
    ENV.install(env)
}

/// The installed settings (the defaults in a unit test).
pub(crate) fn env() -> &'static GpuEnv {
    ENV.get()
}
