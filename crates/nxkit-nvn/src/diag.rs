//! Opt-in diagnostics that the crates of the GPU path share, and the render health report.
//!
//! `NXKIT_DIAG` is a comma separated list of categories (`all` turns every one on). [`on`] reads it once. Every
//! category, knob and `NXKIT_*` variable is listed in `docs/diagnostics.md`.
//!
//! The health report (category `health`) answers "does everything render". Every place that drops, refuses or
//! degrades work counts it under a [`Reason`] with [`note`] or [`note_n`], always, at the cost of one relaxed atomic
//! add. With the category on, [`note`] keeps the first examples of each reason, and [`frame_end`] prints one block
//! every [`REPORT_EVERY`] frames, and one line the frame a reason shows up for the first time.

use std::fmt::{self, Write as _};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// Whether `NXKIT_DIAG` names `cat` or `all`.
pub fn on(cat: &str) -> bool {
    /// `None` for `all`, else the categories. Parsed once: the call comes once per draw on some paths.
    static V: OnceLock<Option<Vec<String>>> = OnceLock::new();
    let v = V.get_or_init(|| {
        let raw = std::env::var("NXKIT_DIAG").unwrap_or_default();
        let list: Vec<String> = raw.split(',').map(str::trim).filter(|c| !c.is_empty()).map(String::from).collect();
        (!list.iter().any(|c| c == "all")).then_some(list)
    });
    v.as_ref().is_none_or(|list| list.iter().any(|c| c == cat))
}

/// True for the first `max` calls with `counter`: the rate limit of a print that can come once per draw or frame.
pub fn first(counter: &AtomicU32, max: u32) -> bool {
    counter.load(Ordering::Relaxed) < max && counter.fetch_add(1, Ordering::Relaxed) < max
}

/// True every `every`th call with `counter`, starting with the first.
pub fn every(counter: &AtomicU32, every: u32) -> bool {
    counter.fetch_add(1, Ordering::Relaxed).is_multiple_of(every.max(1))
}

/// Whether the health report is on (`NXKIT_DIAG=health`).
pub fn health_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| on("health"))
}

/// The frames between two health reports.
pub const REPORT_EVERY: u64 = 300;

/// The examples that the report keeps for each reason.
const EXAMPLES: usize = 3;

/// The longest example that the report keeps, in bytes.
const EXAMPLE_BYTES: usize = 160;

macro_rules! reasons {
    ($($(#[doc = $doc:literal])* $v:ident => $name:literal,)*) => {
        /// A kind of work that was dropped, refused or degraded. The name says where: `skip.*` are the draws, copies
        /// and dispatches that the encoder left out (`nxkit_gpu::Skip`), the others are the causes found on the way.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Reason { $($(#[doc = $doc])* $v,)* }

        impl Reason {
            /// Every reason, in report order.
            pub const ALL: &[Reason] = &[$(Reason::$v,)*];

            /// The short name of the report.
            pub fn name(self) -> &'static str {
                match self { $(Reason::$v => $name,)* }
            }
        }
    };
}

reasons! {
    /// Encoder: the frame has no detail for a command (`Skip::Detail`).
    SkipDetail => "skip.detail",
    /// Encoder: the shader library or the reflection is missing (`Skip::Program`).
    SkipProgram => "skip.program",
    /// Encoder: a binding kind that the encoder does not fill (`Skip::Binding`).
    SkipBinding => "skip.binding",
    /// Encoder: a primitive type without a Metal equivalent (`Skip::Primitive`).
    SkipPrimitive => "skip.primitive",
    /// Encoder: an index type without a Metal equivalent (`Skip::Index`).
    SkipIndex => "skip.index",
    /// Encoder: a vertex, texture or attachment format without a Metal equivalent (`Skip::Format`).
    SkipFormat => "skip.format",
    /// Encoder: the render targets cannot form a pass (`Skip::Target`).
    SkipTarget => "skip.target",
    /// Encoder: front and back faces are culled (`Skip::Cull`). The hardware draws nothing either, so it does not
    /// make the report unclean.
    SkipCull => "skip.cull",
    /// Encoder: a texture could not be created or resolved (`Skip::Texture`).
    SkipTexture => "skip.texture",
    /// Encoder: the pipeline did not compile (`Skip::Pipeline`).
    SkipPipeline => "skip.pipeline",
    /// Encoder: a buffer outside guest memory, or the scratch buffer is full (`Skip::Buffer`).
    SkipBuffer => "skip.buffer",
    /// Encoder: a copy between textures that cannot be copied (`Skip::Copy`).
    SkipCopy => "skip.copy",
    /// Encoder: a texture upload or copy-back failed (`Skip::Transfer`).
    SkipTransfer => "skip.transfer",
    /// Encoder: Metal refused an encoded command (`Skip::Encode`).
    SkipEncode => "skip.encode",
    /// Encoder: the pipeline still compiles on a background thread, the draw waits for it (`Skip::Pending`). It is
    /// a draw that shows a frame or two late, not a loss.
    SkipPending => "skip.pending",
    /// Walker: a draw record without draw arguments (an indirect or unknown draw kind), so the draw has no detail.
    DrawNoArgs => "walk.draw-no-args",
    /// Walker: a draw without a vertex stage in the bound program.
    DrawNoVertex => "walk.draw-no-vertex",
    /// Walker: a program stage that the encoder ignores (geometry, tessellation).
    StageIgnored => "walk.stage-ignored",
    /// Decoder: a texture handle or address of a recording names no texture.
    DecodeUnresolved => "decode.unresolved",
    /// Decoder: a record could not be read, the rest of the recording is lost.
    DecodeBadRecord => "decode.bad-record",
    /// Encoder: a texture handle has no descriptor or no texture record.
    HandleUnresolved => "tex.handle-unresolved",
    /// Encoder: a texture format has no Metal equivalent.
    FormatUnsupported => "tex.format-unsupported",
    /// Encoder: Metal did not create a texture.
    TextureCreate => "tex.create-failed",
    /// Encoder: Metal did not create a texture view (a view between a compressed and an uncompressed format, a
    /// type that the texture cannot be seen as).
    ViewCreate => "tex.view-failed",
    /// Upload: the deswizzle of a block-linear texture failed or was refused.
    UploadFailed => "upload.failed",
    /// Upload: a pitch-linear texture was refused (layers, levels or a stride that the copy does not take).
    UploadLinearRefused => "upload.linear-refused",
    /// Upload: guest data that does not reach a scaled texture.
    UploadScaledLost => "upload.scaled-lost",
    /// Upload: guest data that reached a scaled texture through a staging texture and a GPU scale. This is not a
    /// loss; it shows how often the CPU writes a texture that the GPU renders at the scaled size.
    UploadScaled => "upload.scaled",
    /// Copy-back: a GPU-written texture in CPU-visible memory that is not copied back.
    CopybackRefused => "copyback.refused",
    /// Copy-back: a texture left out because the frame went over `NXKIT_COPYBACK_MAX_MB`.
    CopybackOverBudget => "copyback.over-budget",
    /// Shaders: a program that did not translate or load.
    ProgramFailed => "program.failed",
    /// Pipelines: a render, compute or depth-stencil state that Metal did not build.
    PipelineFailed => "pipeline.failed",
}

impl Reason {
    /// False for a reason that counts work that the hardware does not do either.
    pub fn is_loss(self) -> bool {
        self != Reason::SkipCull && self != Reason::SkipPending && self != Reason::UploadScaled
    }
}

const N: usize = Reason::ALL.len();

/// The counters since the start, and their values at the last report.
struct Counters {
    total: [AtomicU64; N],
    reported: [AtomicU64; N],
}

static COUNTERS: Counters = Counters {
    total: [const { AtomicU64::new(0) }; N],
    reported: [const { AtomicU64::new(0) }; N],
};

/// The first examples of each reason.
static SAMPLES: Mutex<Vec<Vec<String>>> = Mutex::new(Vec::new());

/// Counts one dropped piece of work. With the report on, `example` (format, size, shader key) is kept for the first
/// few of each reason; it is only formatted then.
pub fn note(why: Reason, example: fmt::Arguments<'_>) {
    note_n(why, 1);
    if health_on() {
        keep(why, example);
    }
}

/// Keeps `example` for `why` without counting: for a reason that is counted elsewhere, as the skips are from the
/// counters of the encoder.
pub fn example(why: Reason, example: fmt::Arguments<'_>) {
    if health_on() {
        keep(why, example);
    }
}

/// Counts `n` dropped pieces of work without an example.
pub fn note_n(why: Reason, n: u64) {
    if n != 0 {
        COUNTERS.total[why as usize].fetch_add(n, Ordering::Relaxed);
    }
}

fn keep(why: Reason, example: fmt::Arguments<'_>) {
    let Ok(mut s) = SAMPLES.lock() else { return };
    if s.len() < N {
        s.resize_with(N, Vec::new);
    }
    let list = &mut s[why as usize];
    if list.len() < EXAMPLES {
        let mut text = example.to_string();
        if text.len() > EXAMPLE_BYTES {
            let end = (0..=EXAMPLE_BYTES).rev().find(|i| text.is_char_boundary(*i)).unwrap_or(0);
            text.truncate(end);
            text.push_str("...");
        }
        if !text.is_empty() && !list.contains(&text) {
            list.push(text);
        }
    }
}

/// The count of `why` since the start.
pub fn total(why: Reason) -> u64 {
    COUNTERS.total[why as usize].load(Ordering::Relaxed)
}

/// Ends frame `frame` for the report. It prints the full report every [`REPORT_EVERY`] frames and a short one when a
/// reason counts for the first time. Without `NXKIT_DIAG=health` it does nothing.
pub fn frame_end(frame: u64) {
    if !health_on() {
        return;
    }
    let fresh = Reason::ALL.iter().any(|r| {
        COUNTERS.reported[*r as usize].load(Ordering::Relaxed) == 0 && total(*r) != 0
    });
    if fresh || frame.is_multiple_of(REPORT_EVERY) {
        eprint!("{}", report(frame));
    }
}

/// The report of frame `frame`, and the counters move on to the next report. See `docs/diagnostics.md`.
pub fn report(frame: u64) -> String {
    let samples = SAMPLES.lock().map(|s| s.clone()).unwrap_or_default();
    let mut out = String::new();
    let mut lines = 0;
    for r in Reason::ALL {
        let total = total(*r);
        let before = COUNTERS.reported[*r as usize].swap(total, Ordering::Relaxed);
        if total == 0 {
            continue;
        }
        lines += 1;
        let new = if before == 0 { " NEW" } else { "" };
        let _ = write!(out, "health:   {:<24} {:>10} {:>10}{new}", r.name(), total - before, total);
        if let Some(ex) = samples.get(*r as usize).filter(|e| !e.is_empty()) {
            let _ = write!(out, "  e.g. {}", ex.join(" | "));
        }
        out.push('\n');
    }
    if lines == 0 {
        return format!("health: clean (frame {frame})\n");
    }
    let losses = Reason::ALL.iter().any(|r| r.is_loss() && total(*r) != 0);
    let head = if losses { "work was dropped" } else { "clean apart from culled draws" };
    format!("health: frame {frame}: {head}\nhealth:   {:<24} {:>10} {:>10}\n{out}", "reason", "since last", "total")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reason_has_a_distinct_name() {
        let mut names: Vec<_> = Reason::ALL.iter().map(|r| r.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Reason::ALL.len());
    }

    #[test]
    fn the_report_lists_the_counted_reasons_with_the_change_since_the_last_report() {
        note_n(Reason::UploadLinearRefused, 3);
        let text = report(300);
        assert!(text.contains("upload.linear-refused"), "{text}");
        assert!(text.contains("NEW"), "{text}");
        let again = report(600);
        assert!(again.contains("upload.linear-refused"), "{again}");
        assert!(!again.lines().any(|l| l.contains("upload.linear-refused") && l.contains("NEW")), "{again}");
    }
}
