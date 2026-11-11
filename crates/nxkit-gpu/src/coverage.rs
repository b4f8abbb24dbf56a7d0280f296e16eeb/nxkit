//! How well the build's pipeline archive covers what the game draws (`NXKIT_DIAG=stats`, `health`, `pipe`).
//!
//! Every pipeline the host needs is either found in the archive (a hit) or compiled by Metal (a miss: the draw waits
//! as `skip.pending` and the player sees a hitch). This counts both, adds up the compile time, and with a diagnostic
//! on it asks the archive why a miss missed (`PipelineCache::explain_miss`): the pair of shaders is not archived at
//! all, or it is archived and the state of the draw (attachment formats, blend, vertex layout, sample count, ...)
//! differs from the state the build assumed. The reasons are what the build has to precompile.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use nxkit_metal::{PipelineCache, RenderPipelineDesc, Source};
use nxkit_nvn::diag::{self, Reason};

static ARCHIVE_HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);
static COMPILE_NS: AtomicU64 = AtomicU64::new(0);

/// The misses by cause, counted only with a diagnostic on (the asking costs a few archive lookups each).
static WHY: Mutex<BTreeMap<String, u64>> = Mutex::new(BTreeMap::new());

/// Whether a miss is explained: `stats`, `health` or `pipe` is on.
fn explaining() -> bool {
    crate::diag("stats") || diag::health_on() || crate::diag("pipe")
}

/// Counts one pipeline lookup of the render path. `desc` is the render description, `None` for a mesh pipeline.
pub(crate) fn record(
    cache: &PipelineCache,
    source: Source,
    desc: Option<&RenderPipelineDesc<'_>>,
    what: &str,
) {
    // A specialization of an unspecialized pipeline counts as a hit: the functions were not compiled for the draw.
    let Source::Compiled(took) = source else {
        if matches!(source, Source::Archive(_) | Source::Specialized(_)) {
            ARCHIVE_HITS.fetch_add(1, Ordering::Relaxed);
        }
        return;
    };
    MISSES.fetch_add(1, Ordering::Relaxed);
    COMPILE_NS.fetch_add(took.as_nanos() as u64, Ordering::Relaxed);
    if !explaining() {
        diag::note_n(Reason::PipelineMiss, 1);
        return;
    }
    let why = match desc {
        None => "mesh (not in the build's archive)".to_string(),
        Some(_) if !cache.has_archive() => "no archive".to_string(),
        Some(d) => match cache.explain_miss(d) {
            Ok(w) if w.state_checked && w.state_archived => {
                "state base archived, the lookup or the specialization failed".to_string()
            }
            Ok(w) if w.state_checked && !w.state_differs.is_empty() => format!(
                "state base not archived, the archive holds the pair with another {}",
                w.state_differs.join("+")
            ),
            Ok(w) if w.state_checked && !w.pair_archived => {
                "state base not archived, no base of the pair in any state".to_string()
            }
            Ok(w) if !w.pair_archived => "pair not archived".to_string(),
            Ok(w) if w.rejected.is_empty() => {
                "pair archived, only the whole state misses".to_string()
            }
            Ok(w) => format!("pair archived, state differs: {}", w.rejected.join("+")),
            Err(e) => format!("not explained: {e}"),
        },
    };
    diag::note(
        Reason::PipelineMiss,
        format_args!("{what}: {why} ({:.1} ms)", took.as_secs_f64() * 1e3),
    );
    *WHY.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(why)
        .or_insert(0) += 1;
}

/// The counts of this run: archive hits, misses and the compile time of the misses in nanoseconds.
pub(crate) fn counts() -> (u64, u64, u64) {
    (
        ARCHIVE_HITS.load(Ordering::Relaxed),
        MISSES.load(Ordering::Relaxed),
        COMPILE_NS.load(Ordering::Relaxed),
    )
}

/// The lines of the `stats` report: the totals, then one line for each cause.
pub(crate) fn describe() -> String {
    let (hits, misses, ns) = counts();
    let ms = ns as f64 / 1e6;
    let avg = if misses == 0 { 0.0 } else { ms / misses as f64 };
    let total = hits + misses;
    let pct = if total == 0 {
        100.0
    } else {
        hits as f64 * 100.0 / total as f64
    };
    let mut out = format!(
        "pipelines: archive hits {hits}, misses {misses} ({pct:.1}% covered), compiled in {ms:.0} ms ({avg:.1} ms each)"
    );
    for (why, n) in WHY.lock().unwrap_or_else(PoisonError::into_inner).iter() {
        out.push_str(&format!("\nnxkit-gpu:   miss {n:>6}  {why}"));
    }
    out
}
