//! `NXKIT_DIAG=gateseq`: the order of the calls that matter to the queue gate, for one frame in 300.
//!
//! Every line is `GATESEQ f<frame> +<ms> <what>`, with the milliseconds since the last present returned. The lines
//! come in the order of the calls: submits (queue, recordings and the waits and signals they hold), queue fences and
//! waits, CPU event signals, CPU waits, the decisions of the gate (`HOLD`, `RELEASE`, `FORCE`, `RUN`) with the wait
//! and the one who would satisfy it, every flush with its reason and the commands it carried, and the present.

use std::collections::HashMap;
use std::fmt::Arguments;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

/// One frame in this many is traced.
const EVERY: u64 = 300;

/// Whether the category is on. The hooks for the batches of the GPU are set by [`super::flushstat`].
pub(crate) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        nxkit_nvn::diag::on("gateseq")
    })
}

static ACTIVE: AtomicBool = AtomicBool::new(false);
static FRAME: AtomicU64 = AtomicU64::new(0);
/// The number and start of the frame that is traced (kept until the next traced frame, so that the batches of it that
/// complete later get the same clock).
static TRACED: AtomicU64 = AtomicU64::new(0);
static TRACE_START: AtomicU64 = AtomicU64::new(0);
static START: AtomicU64 = AtomicU64::new(0);

use super::flushstat::now_ns;

/// ms of the host clock `ns` on the clock of the trace (since the start of the traced frame).
fn rel_ms(ns: u64) -> f64 {
    (ns as f64 - TRACE_START.load(Ordering::Relaxed) as f64) / 1e6
}

/// A short number for the calling thread (`t0` is the first thread that traced).
fn tid() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    thread_local! {
        static ID: u64 = NEXT.fetch_add(1, Ordering::Relaxed);
    }
    ID.with(|i| *i)
}

/// Batches of the traced frame: the fences that were committed for it, and the completions that were not matched yet.
#[derive(Default)]
struct Batches {
    /// Batch number to fence, for the batches committed while a frame was traced and not completed yet.
    waiting: HashMap<u64, u64>,
    /// Completions that came before their commit was noted: number to (cbs, start s, end s).
    early: HashMap<u64, (usize, f64, f64, Option<String>)>,
}

static BATCHES: Mutex<Option<Batches>> = Mutex::new(None);

fn batches<R>(f: impl FnOnce(&mut Batches) -> R) -> R {
    let mut g = BATCHES.lock().unwrap_or_else(PoisonError::into_inner);
    f(g.get_or_insert_with(Batches::default))
}

fn print_batch(number: u64, fence: u64, cbs: usize, start_s: f64, end_s: f64, flush: Option<String>) {
    let (s, e) = (start_s * 1e3, end_s * 1e3);
    let base = TRACE_START.load(Ordering::Relaxed) as f64 / 1e6;
    line(format_args!(
        "BATCH {number} (fence {fence}) completed: {cbs} command buffers, GPU start +{:.2} end +{:.2} ({:.2} ms on the GPU){}",
        s - base,
        e - base,
        e - s,
        flush.map_or(String::new(), |f| format!(" | {f}"))
    ));
}

/// A batch was committed for `fence` (the hook of the backend). It is waited for when it belongs to a traced frame.
pub(super) fn batch_kept(fence: u64, number: u64) {
    if !active() {
        return;
    }
    let early = batches(|b| {
        let e = b.early.remove(&number);
        if e.is_none() {
            b.waiting.insert(number, fence);
        }
        e
    });
    if let Some((cbs, s, e, flush)) = early {
        print_batch(number, fence, cbs, s, e, flush);
    }
}

/// A batch completed on the GPU (the hook of the metal crate): printed when it belongs to a traced frame, also after
/// the frame ended.
pub(super) fn batch_done(number: u64, cbs: usize, start_s: f64, end_s: f64, flush: Option<String>) {
    let fence = batches(|b| {
        let f = b.waiting.remove(&number);
        if f.is_none() {
            if b.early.len() > 64 {
                b.early.clear();
            }
            b.early.insert(number, (cbs, start_s, end_s, flush.clone()));
        }
        f
    });
    if let Some(f) = fence {
        print_batch(number, f, cbs, start_s, end_s, flush);
    }
}

/// Who signalled last, by the word of the event counter or by the sync, and when (ns since [`base`]).
#[derive(Default)]
struct Last {
    gpu: HashMap<u64, (u64, u64)>,
    cpu: HashMap<u64, u64>,
    fence: HashMap<u64, (u64, u64)>,
}

static LAST: Mutex<Option<Last>> = Mutex::new(None);

fn last<R>(f: impl FnOnce(&mut Last) -> R) -> R {
    let mut g = LAST.lock().unwrap_or_else(PoisonError::into_inner);
    f(g.get_or_insert_with(Last::default))
}

/// True while a traced frame is open: the caller may do work that only the trace needs.
pub(crate) fn active() -> bool {
    on() && ACTIVE.load(Ordering::Relaxed)
}

fn line(args: Arguments<'_>) {
    eprintln!(
        "GATESEQ f{} t{} +{:.2} {args}",
        TRACED.load(Ordering::Relaxed),
        tid(),
        rel_ms(now_ns())
    );
}

/// One line of the trace.
pub(crate) fn note(args: Arguments<'_>) {
    if active() {
        line(args);
    }
}

/// Runs `body` and traces its return as a second line with the duration, for a call that waits.
pub(crate) fn timed<T: std::fmt::Debug>(what: &str, body: impl FnOnce() -> T) -> T {
    if !active() {
        return body();
    }
    let t = now_ns();
    let v = body();
    note(format_args!(
        "{what} RETURNS {v:?} after {:.2} ms",
        (now_ns() - t) as f64 / 1e6
    ));
    v
}

/// The present returned: the next frame starts, and is traced when its number is a multiple of [`EVERY`].
pub(crate) fn end_frame() {
    if !on() {
        return;
    }
    note(format_args!("end of frame"));
    let n = FRAME.fetch_add(1, Ordering::Relaxed) + 1;
    let t = now_ns();
    START.store(t, Ordering::Relaxed);
    let traced = n.is_multiple_of(EVERY);
    if traced {
        TRACED.store(n, Ordering::Relaxed);
        TRACE_START.store(t, Ordering::Relaxed);
    }
    ACTIVE.store(traced, Ordering::Relaxed);
}

/// A queue's GPU work that ran signals the event whose counter is at `word`.
pub(crate) fn gpu_signal(word: u64, queue: u64) {
    if on() {
        last(|l| l.gpu.insert(word, (queue, now_ns())));
    }
}

/// The CPU signalled the event whose counter is at `word`.
pub(crate) fn cpu_signal(word: u64) {
    if on() {
        last(|l| l.cpu.insert(word, now_ns()));
    }
}

/// A queue's GPU work that ran fences `sync`.
pub(crate) fn fenced(sync: u64, queue: u64) {
    if on() {
        last(|l| l.fence.insert(sync, (queue, now_ns())));
    }
}

fn ago(t: u64) -> String {
    format!("+{:.2} ms", rel_ms(t))
}

/// Who signalled the event counter at `word` last, for a line about a wait on it.
pub(crate) fn last_event_signals(word: u64) -> String {
    last(|l| {
        let g = l
            .gpu
            .get(&word)
            .map_or("none".to_string(), |(q, t)| format!("q {q:#x} at {}", ago(*t)));
        let c = l
            .cpu
            .get(&word)
            .map_or("none".to_string(), |t| format!("at {}", ago(*t)));
        format!("last GPU signal {g}, last CPU signal {c}")
    })
}

/// Who fenced `sync` last.
pub(crate) fn last_fence(sync: u64) -> String {
    last(|l| {
        l.fence
            .get(&sync)
            .map_or("never fenced by a queue that ran".to_string(), |(q, t)| {
                format!("last fenced by q {q:#x} at {}", ago(*t))
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_signals_are_remembered_by_word_and_sync() {
        // The state is global and the category is off in tests, so the recorders do nothing; the formatting of an
        // unknown word and sync is what a trace shows for a wait nobody signalled.
        gpu_signal(1, 2);
        cpu_signal(1);
        fenced(3, 2);
        assert_eq!(
            last_event_signals(0xdead),
            "last GPU signal none, last CPU signal none"
        );
        assert_eq!(last_fence(0xbeef), "never fenced by a queue that ran");
        assert!(!active());
    }
}
