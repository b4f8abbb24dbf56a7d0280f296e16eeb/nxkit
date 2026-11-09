//! The counters of the pipeline builds and the `pipe` diagnostic line (`NXKIT_DIAG=pipe`, every 60 frames).
//!
//! The line tells where a backlog of `skip.pending` comes from: how many draws waited, how many jobs were queued, how
//! many wait and compile now, and what each kind of pipeline creation cost (an exact archive hit, a specialization of
//! an unspecialized pipeline, a compile from the functions) with its mean and longest time in the window.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use nxkit_metal::{PipelineCache, Source};

/// The frame the encoder is at, which orders the queue.
static FRAME: AtomicU64 = AtomicU64::new(0);
/// Draws left out because their pipeline was not ready.
static PENDING_DRAWS: AtomicU64 = AtomicU64::new(0);
/// Jobs queued.
static QUEUED: AtomicU64 = AtomicU64::new(0);
/// Jobs whose pipeline Metal refused (or whose compile panicked).
static FAILED: AtomicU64 = AtomicU64::new(0);
/// Jobs waiting, compiling, and the threads.
static WAITING: AtomicU64 = AtomicU64::new(0);
static RUNNING: AtomicU64 = AtomicU64::new(0);
static THREADS: AtomicU64 = AtomicU64::new(0);

/// The kinds of a finished job, in the order of [`KINDS`].
const KINDS: [&str; 4] = ["cached", "archive", "specialized", "compiled"];

/// Per kind: the count, the sum of the times in nanoseconds and the longest time in the window.
static COUNT: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
static SUM_NS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
static MAX_NS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

/// The cache whose specialization counters the line shows.
static CACHE: Mutex<Option<Weak<PipelineCache>>> = Mutex::new(None);

/// The totals at the last line, so the line shows the window.
static LAST: Mutex<[u64; 3]> = Mutex::new([0; 3]);

/// The frames between two lines.
const EVERY: u64 = 60;

pub(super) fn frame() -> u64 {
    FRAME.load(Ordering::Relaxed)
}

pub(super) fn pending_draw() {
    PENDING_DRAWS.fetch_add(1, Ordering::Relaxed);
}

pub(super) fn queued() {
    QUEUED.fetch_add(1, Ordering::Relaxed);
}

pub(super) fn failed() {
    FAILED.fetch_add(1, Ordering::Relaxed);
}

pub(super) fn queue(waiting: usize, running: usize, threads: usize) {
    WAITING.store(waiting as u64, Ordering::Relaxed);
    RUNNING.store(running as u64, Ordering::Relaxed);
    THREADS.store(threads as u64, Ordering::Relaxed);
}

pub(super) fn watch(cache: &Arc<PipelineCache>) {
    *CACHE.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::downgrade(cache));
}

/// Counts a finished job by where its pipeline came from. `wall` is the whole job, the descriptor included.
pub(super) fn made(source: Source, wall: Duration) {
    let i = match source {
        Source::Cached => 0,
        Source::Archive(_) => 1,
        Source::Specialized(_) => 2,
        Source::Compiled(_) => 3,
    };
    let ns = wall.as_nanos() as u64;
    COUNT[i].fetch_add(1, Ordering::Relaxed);
    SUM_NS[i].fetch_add(ns, Ordering::Relaxed);
    MAX_NS[i].fetch_max(ns, Ordering::Relaxed);
}

/// Marks the end of frame `n`; under `NXKIT_DIAG=pipe` prints the line every [`EVERY`] frames.
pub(crate) fn frame_done(n: u64) {
    FRAME.store(n, Ordering::Relaxed);
    if n.is_multiple_of(EVERY) && crate::diag("pipe") {
        eprintln!("DIAG pipe frame {n}: {}", line());
    }
}

/// The text of the line: the window since the line before, then the state now.
fn line() -> String {
    let now = [
        PENDING_DRAWS.load(Ordering::Relaxed),
        QUEUED.load(Ordering::Relaxed),
        FAILED.load(Ordering::Relaxed),
    ];
    let before = std::mem::replace(
        &mut *LAST.lock().unwrap_or_else(PoisonError::into_inner),
        now,
    );
    let mut out = format!(
        "pending draws {} (total {}), new jobs {}, failed {} | queue {}, compiling {} of {} threads |",
        now[0] - before[0],
        now[0],
        now[1] - before[1],
        now[2] - before[2],
        WAITING.load(Ordering::Relaxed),
        RUNNING.load(Ordering::Relaxed),
        THREADS.load(Ordering::Relaxed),
    );
    for (i, kind) in KINDS.iter().enumerate() {
        let n = COUNT[i].swap(0, Ordering::Relaxed);
        let sum = SUM_NS[i].swap(0, Ordering::Relaxed);
        let max = MAX_NS[i].swap(0, Ordering::Relaxed);
        if n > 0 {
            out.push_str(&format!(
                " {kind} {n} ({:.1}/{:.1} ms mean/max)",
                sum as f64 / n as f64 / 1e6,
                max as f64 / 1e6
            ));
        } else {
            out.push_str(&format!(" {kind} 0"));
        }
    }
    let cache = CACHE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .and_then(Weak::upgrade);
    if let Some(c) = cache {
        let b = c.base_stats();
        out.push_str(&format!(
            " | bases from archive {} compiled {} missing {}, specialized {} refused {}",
            b.found, b.compiled, b.missing, b.specialized, b.failed
        ));
        if let Some(e) = b.first_error {
            out.push_str(&format!(" (first: {e})"));
        }
    }
    out
}
