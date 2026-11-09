//! The opt-in diagnostics of the encoder: categories, knobs, filters and the sampled frame.
//!
//! Everything here is off unless an environment variable turns it on, and costs one cached check otherwise.
//! `docs/diagnostics.md` lists every category, knob and variable.
//!
//! - [`diag`]: `NXKIT_DIAG`, a comma separated list of categories (`all` for every one), shared with `nxkit-nvn` and
//!   `nxkit-rt` through [`nxkit_nvn::diag::on`].
//! - [`knob`]: `NXKIT_KNOB`, experiments that change what the encoder does. `NXKIT_KNOB_FILE` names a file with the
//!   same list that is read again every frame, so a knob can be flipped while the game runs.
//! - [`nobar`]: the file `nobar` in the diagnostics directory holds a pass range ("lo hi") that gets full barriers,
//!   for bisecting a barrier problem. It is read every frame too.
//! - [`tex_match`]: `NXKIT_DIAG_TEX` narrows the texture and pass categories to the textures of one format or size.
//! - [`sampled`]: the per-draw categories print on one frame out of [`SAMPLE_EVERY`], so they stay readable.
//! - [`watched`]: `NXKIT_WATCH_TEX` names texture ids whose passes and copies are printed.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use nxkit_metal::Format;
use nxkit_nvn::resource::TexId;

use crate::stats::Skip;

/// Whether `NXKIT_DIAG` names `cat` or `all`.
pub(crate) fn diag(cat: &str) -> bool {
    nxkit_nvn::diag::on(cat)
}

/// The directory of the diagnostic files: `NXKIT_DIAG_DIR`, `/tmp/fmap` without it. The `nobar` file is read from it,
/// the `img` pictures are written into it.
pub(crate) fn diag_dir() -> &'static PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        std::env::var_os("NXKIT_DIAG_DIR").map_or_else(|| PathBuf::from("/tmp/fmap"), PathBuf::from)
    })
}

/// The knobs, by name. Each one changes what the encoder does, for experiments while debugging a game. They are not
/// meant to render correctly.
const KNOBS: &[&str] = &[
    // The depth and stencil tests are off.
    "nodepth",
    // Culling is off.
    "nocull",
    // The depth bias is zero.
    "nooffset",
    // A draw into four colour targets that the fragment shader all writes gets constant G-buffer values.
    "flat",
    // The depth test `equal` becomes `always`.
    "noeq",
    // Every draw into one colour target is painted magenta, or only the fragment programs of `NXKIT_PAINT_FS`.
    "paint",
    // Like `paint`, with the absolute value of the first varying as the colour.
    "paintdir",
    // A cube view is made as a cube array.
    "cubearr",
    // Every pass gets a full barrier from all earlier work.
    "allbar",
    // No pass gets a barrier. Wrong on purpose: it measures what the barriers cost the GPU.
    "nobarrier",
    // `nvnSyncWait` returns at once and does not wait for the GPU. Wrong on purpose: the game may write memory that the
    // frame on the GPU still reads. It measures what the wait for the newest fence costs.
    "nowait",
    // The same as `NXKIT_OVERLAP=1`, switched while the game runs, to compare both at one place.
    "overlap",
    // With the overlap on, the uniform buffers are not copied. Wrong on purpose: it measures what the copies cost.
    "nosnap",
    // A render pass that samples what an earlier pass wrote holds its vertex stage too, also when no vertex shader of
    // it samples (the behaviour before the barriers looked at the stages of the bindings).
    "vsbar",
    // With the overlap on, the storage buffers that draws only read use the buffer itself instead of the walk's copy
    // (the behaviour before; the CPU's writes for the next frame then race with the GPU).
    "nosnapsbuf",
    // With the overlap and the encoder thread, a wait for the newest frame waits for the frame two back.
    "overlap2",
    // The pass plan keeps render passes that do nothing, and does not merge across a dropped pass or a change of the
    // target set (the behaviour before). For comparing the frame time.
    "nopassmerge",
    // The pass plan does not learn dead stores of scratch targets from the frames before.
    "noloadstore",
    // Opt in: a colour attachment that the first draw of its pass visibly covers whole (one full-screen triangle with
    // no vertex input, full viewport, no blend, no test) loads nothing. The shader may still discard, so it is off.
    "coverload",
    // A render condition whose two operands both are plain guest memory (not a report of the table) renders.
    "rcflag",
    // A render condition with an unknown result is left out instead of rendered.
    "rcunk",
    // Every `SetRenderEnableConditional` renders. `SetRenderEnable` is not changed.
    "rcall",
    // The same as `NXKIT_RENDERCOND_STALE=0`, switched while the game runs: no counter of an earlier report is read.
    "rcnostale",
    // The same as `NXKIT_RENDERCOND=3d`, switched while the game runs: a false condition leaves out draws and clears only.
    "rc3d",
    // A Metal texture that the frame made new and that gets no upload is cleared to zero before the frame uses it.
    "freshzero",
];

/// Whether `NXKIT_KNOB` (or the file of `NXKIT_KNOB_FILE`) names `name`.
pub(crate) fn knob(name: &str) -> bool {
    let bit = knob_bit(name);
    debug_assert!(bit != 0, "unknown knob {name}");
    knobs().flags.load(Ordering::Relaxed) & bit != 0
}

struct Knobs {
    flags: AtomicU32,
    file: Option<PathBuf>,
}

fn knobs() -> &'static Knobs {
    static K: OnceLock<Knobs> = OnceLock::new();
    K.get_or_init(|| Knobs {
        flags: AtomicU32::new(set_pass_options(parse_knobs(
            &std::env::var("NXKIT_KNOB").unwrap_or_default(),
        ))),
        file: std::env::var_os("NXKIT_KNOB_FILE").map(PathBuf::from),
    })
}

/// Hands the knobs that `nxkit-nvn` acts on to it: the two of the pass optimiser, which makes the plan, and the five
/// of the render conditions, which it decides on the guest thread. Returns `flags`.
fn set_pass_options(flags: u32) -> u32 {
    use nxkit_nvn::rendercond::force;
    nxkit_nvn::passes::set_options(
        flags & knob_bit("nopassmerge") == 0,
        flags & knob_bit("noloadstore") == 0,
    );
    let on = |name: &str, bit: u8| if flags & knob_bit(name) != 0 { bit } else { 0 };
    force::set_force(
        on("rcflag", force::RCFLAG)
            | on("rcunk", force::RCUNK)
            | on("rcall", force::RCALL)
            | on("rcnostale", force::NOSTALE)
            | on("rc3d", force::ONLY_3D),
    );
    flags
}

fn knob_bit(name: &str) -> u32 {
    KNOBS.iter().position(|k| *k == name).map_or(0, |i| 1 << i)
}

fn parse_knobs(list: &str) -> u32 {
    list.split(',')
        .map(|s| knob_bit(s.trim()))
        .fold(0, |a, b| a | b)
}

/// The pass range `[lo, hi)` of the `nobar` file.
static NOBAR: Mutex<Option<(usize, usize)>> = Mutex::new(None);

/// The pass range of the `nobar` file, for the counters line.
pub(crate) fn nobar_range() -> Option<(usize, usize)> {
    NOBAR.lock().ok().and_then(|r| *r)
}

/// True when pass `index` lies in the range of the `nobar` file.
pub(crate) fn nobar(index: usize) -> bool {
    nobar_range().is_some_and(|(lo, hi)| (lo..hi).contains(&index))
}

/// Reads the `nobar` file and the knob file again. The encoder calls it once per frame. Without `NXKIT_KNOB_FILE`
/// and without the `nobar` file it costs one failed open.
pub(crate) fn refresh_knobs() {
    if let Ok(mut r) = NOBAR.lock() {
        *r = std::fs::read_to_string(diag_dir().join("nobar"))
            .ok()
            .and_then(|t| {
                let mut it = t.split_whitespace().filter_map(|w| w.parse::<usize>().ok());
                Some((it.next()?, it.next()?))
            });
    }
    let k = knobs();
    if let Some(value) = k
        .file
        .as_ref()
        .and_then(|f| std::fs::read_to_string(f).ok())
    {
        k.flags
            .store(set_pass_options(parse_knobs(&value)), Ordering::Relaxed);
    }
}

/// The fragment program keys of `NXKIT_PAINT_FS` (comma separated) that the `paint` knobs paint. Empty paints every
/// draw into one colour target.
pub(crate) fn paint_programs() -> &'static [String] {
    static KEYS: OnceLock<Vec<String>> = OnceLock::new();
    KEYS.get_or_init(|| {
        std::env::var("NXKIT_PAINT_FS")
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// The texture filter of `NXKIT_DIAG_TEX`: a format name (as `Debug` prints it), a size `WxH` or `WxHxL` (layers),
/// or both, separated by `/` (`R16Unorm/256x256`). Every part that is given has to match.
#[derive(Debug, Default)]
struct TexFilter {
    format: Option<String>,
    size: Option<[u32; 2]>,
    layers: Option<u32>,
}

fn tex_filter() -> Option<&'static TexFilter> {
    static F: OnceLock<Option<TexFilter>> = OnceLock::new();
    F.get_or_init(|| {
        let v = std::env::var("NXKIT_DIAG_TEX").ok()?;
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
        Some(f)
    })
    .as_ref()
}

/// Whether `NXKIT_DIAG_TEX` is set.
pub(crate) fn tex_filtered() -> bool {
    tex_filter().is_some()
}

/// Whether a texture passes `NXKIT_DIAG_TEX`. Without the variable every texture passes.
pub(crate) fn tex_match(format: Format, width: u32, height: u32, layers: u32) -> bool {
    let Some(f) = tex_filter() else { return true };
    f.size.is_none_or(|s| s == [width, height])
        && f.layers.is_none_or(|l| l == layers)
        && f.format
            .as_ref()
            .is_none_or(|n| *n == format!("{format:?}"))
}

/// The frames between two sampled frames.
pub(crate) const SAMPLE_EVERY: u64 = 300;

/// The number of the frame that the encoder works on.
static FRAME: AtomicU64 = AtomicU64::new(0);

/// Starts a frame for the diagnostics and gives its number.
pub(crate) fn begin_frame() -> u64 {
    FRAME.fetch_add(1, Ordering::Relaxed) + 1
}

/// The number of the frame that the encoder works on.
pub(crate) fn frame() -> u64 {
    FRAME.load(Ordering::Relaxed)
}

/// True on the frames that the per-draw categories print: one frame out of [`SAMPLE_EVERY`], from the 150th on, so
/// the first one is past the boot screens.
pub(crate) fn sampled() -> bool {
    frame() % SAMPLE_EVERY == SAMPLE_EVERY / 2
}

/// True for the first `max` calls with `budget` on the current frame. The budget starts again on the next frame.
pub(crate) fn sample_budget(budget: &AtomicU64, max: u32) -> bool {
    let f = frame();
    let v = budget.load(Ordering::Relaxed);
    if v >> 32 != f & 0xffff_ffff {
        budget.store((f & 0xffff_ffff) << 32, Ordering::Relaxed);
    }
    budget.fetch_add(1, Ordering::Relaxed) & 0xffff_ffff < u64::from(max)
}

/// True for the first `max` calls with `counter`.
pub(crate) fn first(counter: &AtomicU32, max: u32) -> bool {
    nxkit_nvn::diag::first(counter, max)
}

/// Prints a line of category `tex` (at most 300) and gives back `Skip::Texture`. The health report keeps it as an
/// example of the skip.
pub(crate) fn tex_skip(what: std::fmt::Arguments<'_>) -> Skip {
    static SHOWN: AtomicU32 = AtomicU32::new(0);
    nxkit_nvn::diag::example(nxkit_nvn::diag::Reason::SkipTexture, what);
    if diag("tex") && first(&SHOWN, 300) {
        eprintln!("DIAG tex {what}");
    }
    Skip::Texture
}

/// Prints a line of category `buf` (at most 300) and gives back `Skip::Buffer`. The health report keeps it as an
/// example of the skip.
pub(crate) fn buf_skip(what: std::fmt::Arguments<'_>) -> Skip {
    static SHOWN: AtomicU32 = AtomicU32::new(0);
    nxkit_nvn::diag::example(nxkit_nvn::diag::Reason::SkipBuffer, what);
    if diag("buf") && first(&SHOWN, 300) {
        eprintln!("DIAG buf {what}");
    }
    Skip::Buffer
}

/// Whether `NXKIT_WATCH_TEX` (a comma separated list of texture ids) names `id`.
pub(crate) fn watched(id: TexId) -> bool {
    static IDS: OnceLock<Vec<u32>> = OnceLock::new();
    IDS.get_or_init(|| {
        std::env::var("NXKIT_WATCH_TEX")
            .map(|v| v.split(',').filter_map(|s| s.trim().parse().ok()).collect())
            .unwrap_or_default()
    })
    .contains(&id.0)
}

/// `NXKIT_GPU_LABELS=1`: the encoders of the passes get names (pass index, first attachment, draws) for Instruments.
pub(crate) fn gpu_labels() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("NXKIT_GPU_LABELS").is_ok_and(|v| v == "1"))
}

/// `NXKIT_SKIP_PROGRAMS=<key>,<key>`: dispatches whose compute shader has one of these keys are not run. A debugging aid
/// to find which pass of a chain turns a picture black, or which pass costs the GPU time.
///
/// `NXKIT_SKIP_FILE=<path>` does the same with the keys in a file (comma or line separated), read again twice a
/// second, so one run can measure one program after the other while the game stays in the same scene.
pub(crate) fn skipped_program(key: &str) -> bool {
    use std::sync::{Mutex, OnceLock, PoisonError};
    use std::time::{Duration, Instant};
    fn parse(v: &str) -> Vec<String> {
        v.split([',', '\n'])
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .collect()
    }
    static KEYS: OnceLock<Vec<String>> = OnceLock::new();
    let keys = KEYS.get_or_init(|| {
        std::env::var("NXKIT_SKIP_PROGRAMS")
            .map(|v| parse(&v))
            .unwrap_or_default()
    });
    if keys.iter().any(|k| k == key) {
        return true;
    }
    static FILE: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    let Some(path) = FILE.get_or_init(|| std::env::var_os("NXKIT_SKIP_FILE").map(Into::into))
    else {
        return false;
    };
    static READ: Mutex<Option<(Instant, Vec<String>)>> = Mutex::new(None);
    let mut read = READ.lock().unwrap_or_else(PoisonError::into_inner);
    let stale = read
        .as_ref()
        .is_none_or(|(at, _)| at.elapsed() > Duration::from_millis(500));
    if stale {
        let keys = std::fs::read_to_string(path)
            .map(|v| parse(&v))
            .unwrap_or_default();
        *read = Some((Instant::now(), keys));
    }
    read.as_ref()
        .is_some_and(|(_, keys)| keys.iter().any(|k| k == key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_knob_has_its_own_bit() {
        assert!(KNOBS.len() <= 32);
        for (i, k) in KNOBS.iter().enumerate() {
            assert_eq!(knob_bit(k), 1 << i);
        }
        assert_eq!(knob_bit("unknown"), 0);
        assert_eq!(
            parse_knobs("nocull, noeq"),
            knob_bit("nocull") | knob_bit("noeq")
        );
    }
}
