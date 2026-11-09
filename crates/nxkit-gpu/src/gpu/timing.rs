//! The time marks of one encode, for `NXKIT_DIAG=hitch`.

use super::*;

/// The nanoseconds from `from` to `to`.
pub(super) fn nanos(from: Instant, to: Instant) -> u64 {
    u64::try_from(to.duration_since(from).as_nanos()).unwrap_or(u64::MAX)
}

/// Notes the time since `t0` under `name`, for the `hitch` line.
pub(super) fn mark(t0: Instant, name: &'static str) {
    MARKS.with(|m| m.borrow_mut().push((name, nanos(t0, Instant::now()))));
}

thread_local! {
    /// The steps of the last encode on this thread and the time since its start, for `NXKIT_DIAG=hitch`.
    pub(super) static MARKS: std::cell::RefCell<Vec<(&'static str, u64)>> = const { std::cell::RefCell::new(Vec::new()) };
    /// The time of the phases of the last encode on this thread, for `NXKIT_DIAG=hitch`: the wait for a free batch, the
    /// copy-back, the residency commit and the submit.
    pub(super) static PHASES: std::cell::RefCell<[u64; 4]> = const { std::cell::RefCell::new([0; 4]) };
}

/// When a chunk of the encode started, how long it ran on the wall clock and on its thread's CPU clock.
#[derive(Clone, Copy, Default)]
pub(super) struct ChunkTiming {
    pub(super) delay_ns: u64,
    pub(super) wall_ns: u64,
    pub(super) cpu_ns: u64,
    pub(super) passes: usize,
}

thread_local! {
    /// The chunks of the last encode on this thread, for `NXKIT_DIAG=hitch`.
    pub(super) static CHUNKS: std::cell::RefCell<Vec<ChunkTiming>> = const { std::cell::RefCell::new(Vec::new()) };
}
