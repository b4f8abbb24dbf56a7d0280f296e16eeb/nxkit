//! The times of the timestamp reports.
//!
//! A report belongs to the frame (fence) that recorded it. When the GPU finished the batch of that fence, the report
//! gets the GPU start (`TIMESTAMP_TOP`) or the GPU end (every other kind) of that batch, in report ticks of the console
//! (`Platform::report_ticks_from_ns`). [`Clock`] keeps the last time written, so a time is never 0 and never goes
//! back: a batch without GPU work, or one that starts before the end of the batch before it, takes the last time.
//!
//! The base of the times is the host clock that Metal reports GPU times on (`CACurrentMediaTime`, which is
//! `mach_absolute_time`). It is the base of the system tick and of `nvnDeviceGetCurrentTimestampInNanoseconds`.

use std::collections::HashMap;

use nxkit_abi::platform;
use nxkit_metal::CommandBufferStats;

use super::*;

/// The `NVNcounterType` of `TIMESTAMP_TOP`.
const TIMESTAMP_TOP: u32 = 0x10;

/// Where a time came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Source {
    /// The times of the batch itself.
    Batch,
    /// The batch has no GPU time (empty, or no batch known), the last time written is kept.
    Kept,
    /// The times of the batch were raised to the last time written.
    Clamped,
}

impl Source {
    fn name(self) -> &'static str {
        match self {
            Source::Batch => "batch",
            Source::Kept => "kept",
            Source::Clamped => "clamped",
        }
    }
}

/// The start and end of the work of one fence, in nanoseconds of the host clock. Both are above 0, and `start_ns` is
/// at most `end_ns`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Times {
    pub(super) start_ns: u64,
    pub(super) end_ns: u64,
    pub(super) source: Source,
}

/// The times of a batch given its GPU start and end in seconds, or `None` when the batch is not known. `last_ns` is
/// the last time written (0 before the first), `now_ns` the host clock now: it stands in when nothing was written yet
/// and the batch has no GPU time.
pub(super) fn resolve(last_ns: u64, batch: Option<(f64, f64)>, now_ns: u64) -> Times {
    let ns = |s: f64| {
        if s.is_finite() && s > 0.0 {
            (s * 1e9) as u64
        } else {
            0
        }
    };
    let (start, end) = batch.map_or((0, 0), |(a, b)| (ns(a), ns(b)));
    if end == 0 {
        let t = if last_ns == 0 { now_ns.max(1) } else { last_ns };
        return Times {
            start_ns: t,
            end_ns: t,
            source: Source::Kept,
        };
    }
    let start = if start == 0 { end } else { start.min(end) };
    let raised_start = start.max(last_ns);
    let raised_end = end.max(raised_start);
    Times {
        start_ns: raised_start,
        end_ns: raised_end,
        source: if (raised_start, raised_end) == (start, end) {
            Source::Batch
        } else {
            Source::Clamped
        },
    }
}

/// The last time written on the queue, and the count of the writes for the diagnostic.
#[derive(Default)]
pub(super) struct Clock {
    last_ns: u64,
    /// The time of the report written last, for the delta of the diagnostic.
    report_ns: u64,
    writes: u64,
}

impl Clock {
    /// The times of the next fence, in the order of the fences. It remembers the end.
    pub(super) fn next(&mut self, batch: Option<(f64, f64)>, now_ns: u64) -> Times {
        let t = resolve(self.last_ns, batch, now_ns);
        self.last_ns = t.end_ns;
        t
    }

    /// `NXKIT_DIAG=gputs`: the line of a report written, for the first 200 and then every 600th.
    fn note(
        &mut self,
        r: &crate::detail::CounterReport,
        fence: u64,
        t: &Times,
        ns: u64,
        which: &str,
    ) {
        let n = self.writes;
        self.writes += 1;
        let delta = i128::from(ns) - i128::from(self.report_ns);
        self.report_ns = ns;
        if crate::diag("gputs") && (n < 200 || n.is_multiple_of(600)) {
            eprintln!(
                "DIAG gputs report addr {:#x} kind {} fence {fence} start/end {which} ns {ns} delta-to-previous-on-queue {delta} source {}",
                r.addr,
                r.kind,
                t.source.name()
            );
        }
    }
}

impl GpuBackend {
    /// Waits for the batches of the fences up to `upto` that were kept and returns the numbers of each, with the fence.
    /// The flag is false when a batch failed; its fence has no entry.
    pub(super) fn finish_batches(&self, upto: u64) -> (Vec<(u64, CommandBufferStats)>, bool) {
        let batches: Vec<_> = {
            let mut q = locked(&self.inflight);
            let (due, keep): (Vec<_>, Vec<_>) = q.drain(..).partition(|(f, _)| *f <= upto);
            *q = keep.into();
            due
        };
        let mut ok = true;
        let mut out = Vec::with_capacity(batches.len());
        for (fence, batch) in &batches {
            match batch.wait() {
                Ok(stats) => out.push((*fence, stats)),
                Err(_) => ok = false,
            }
        }
        (out, ok)
    }

    /// Writes the reports of the frames up to fence `upto` into guest memory, each a 16-byte `NVNcounterData` (the 64-bit
    /// counter, then the 64-bit time in report ticks of the console, `Platform::report_ticks_from_ns`), only where all
    /// 16 bytes lie in guest memory (`queries::write_report`). `finished` holds the numbers of the batches that the GPU
    /// finished, by fence.
    ///
    /// - A `TIMESTAMP_TOP` gets the GPU start of the batch of its own fence and a `TIMESTAMP` its end, with a counter of
    ///   0. The game takes its GPU time from the difference and picks its frame interval from it; without the reports it
    ///   saw no GPU time and kept a 30 fps step on frames that took longer, which ran the game in slow motion. Fences
    ///   that finish together each get their own times. A fence without GPU time takes the last time written.
    /// - A `ReportValue` writes its 32-bit value as the counter (zero-extended, as the GPU writes the payload of a
    ///   four-word report) and the end time. The render conditions of later frames read it (`nxkit_nvn::rendercond`).
    /// - The GPU counters (samples passed, vertices, primitives) are written and landed by `queries`, with the end time
    ///   of their fence.
    ///
    /// The clock lock is held from taking the due reports to the last write, so a wait that finds nothing due returns
    /// only after the wait that took them has written them.
    pub(super) fn write_counters(&self, upto: u64, finished: &[(u64, CommandBufferStats)]) {
        let mut clock = locked(&self.ts);
        let due = take_due(&self.counters, upto);
        let due_queries = take_due(&self.queries, upto);
        let mut fences: Vec<u64> = due
            .iter()
            .map(|(f, _)| *f)
            .chain(due_queries.iter().map(|(f, _)| *f))
            .collect();
        fences.sort_unstable();
        fences.dedup();
        let now = (nxkit_metal::media_time() * 1e9) as u64;
        let mut times = HashMap::with_capacity(fences.len());
        for f in fences {
            let batch = finished
                .iter()
                .find(|(g, _)| *g == f)
                .map(|(_, s)| (s.gpu_start_s, s.gpu_end_s));
            times.insert(f, clock.next(batch, now));
        }
        let platform = platform::get();
        let map = self.guest.address_map();
        for (f, r) in due {
            let t = times[&f];
            let (ns, which) = if r.kind == TIMESTAMP_TOP {
                (t.start_ns, "start")
            } else {
                (t.end_ns, "end")
            };
            let ticks = platform.report_ticks_from_ns(ns);
            if crate::queries::write_report(&self.guest, &map, r.addr, u64::from(r.value), ticks)
                .is_none()
            {
                report_refused(r.addr);
            }
            clock.note(&r, f, &t, ns, which);
        }
        for (f, q) in due_queries {
            let end = platform.report_ticks_from_ns(times[&f].end_ns);
            crate::queries::write_diag(&q.write(&self.guest, end));
        }
        crate::gatediag::landed(upto, &self.guest);
    }

    /// Keeps the batch of a fence until its reports are written. Only the newest [`KEPT_BATCHES`] stay.
    pub(super) fn keep_batch(&self, fence: u64, batch: nxkit_metal::SubmittedBatch) {
        let mut q = locked(&self.inflight);
        nxkit_nvn::diag::batch_kept(fence, batch.number());
        if q.back()
            .is_none_or(|(_, last)| last.number() != batch.number())
        {
            q.push_back((fence, batch));
        }
        while q.len() > KEPT_BATCHES {
            q.pop_front();
        }
    }
}

/// The batches that the backend keeps for the times of the reports.
const KEPT_BATCHES: usize = 32;

#[cfg(test)]
mod tests {
    use super::*;

    const S: f64 = 1e9;

    #[test]
    fn a_normal_batch_gives_its_own_start_and_end() {
        let t = resolve(0, Some((2.0, 2.5)), 99);
        assert_eq!(t.start_ns, 2_000_000_000);
        assert_eq!(t.end_ns, 2_500_000_000);
        assert_eq!(t.source, Source::Batch);
    }

    #[test]
    fn an_empty_batch_keeps_the_last_time_and_is_never_zero() {
        let mut c = Clock::default();
        let first = c.next(Some((4.0, 4.25)), 99);
        assert_eq!(first.source, Source::Batch);
        for batch in [Some((0.0, 0.0)), None] {
            let t = c.next(batch, 99);
            assert_eq!((t.start_ns, t.end_ns), (first.end_ns, first.end_ns));
            assert_eq!(t.source, Source::Kept);
        }
        // Nothing was written yet: the clock of now stands in, and still not 0.
        let t = Clock::default().next(Some((0.0, 0.0)), 7_000);
        assert_eq!((t.start_ns, t.end_ns), (7_000, 7_000));
        let t = Clock::default().next(None, 0);
        assert_eq!((t.start_ns, t.end_ns), (1, 1));
    }

    #[test]
    fn two_fences_in_one_drain_get_their_own_times() {
        let mut c = Clock::default();
        let a = c.next(Some((1.0, 1.25)), 0);
        let b = c.next(Some((1.5, 1.75)), 0);
        assert_eq!((a.start_ns, a.end_ns), (1_000_000_000, 1_250_000_000));
        assert_eq!((b.start_ns, b.end_ns), (1_500_000_000, 1_750_000_000));
        assert_ne!(a.end_ns, b.end_ns);
        assert_eq!(b.end_ns - a.end_ns, (0.5 * S) as u64);
    }

    #[test]
    fn a_batch_that_starts_before_the_previous_end_stays_monotonic() {
        let mut c = Clock::default();
        let a = c.next(Some((1.0, 2.0)), 0);
        // Overlap: it starts at 1.5 and ends at 3.0. Its start is raised to 2.0.
        let b = c.next(Some((1.5, 3.0)), 0);
        assert_eq!((b.start_ns, b.end_ns), (a.end_ns, 3_000_000_000));
        assert_eq!(b.source, Source::Clamped);
        // Entirely before the last time: both are the last time.
        let d = c.next(Some((0.5, 0.75)), 0);
        assert_eq!((d.start_ns, d.end_ns), (b.end_ns, b.end_ns));
        assert_eq!(d.source, Source::Clamped);
        // A later batch is not touched.
        let e = c.next(Some((4.0, 4.5)), 0);
        assert_eq!((e.start_ns, e.end_ns), (4_000_000_000, 4_500_000_000));
        assert_eq!(e.source, Source::Batch);
    }

    #[test]
    fn the_start_is_never_after_the_end() {
        let mut c = Clock::default();
        let mut last = 0;
        for batch in [
            Some((2.0, 3.0)),
            Some((2.5, 2.25)),
            Some((0.0, 5.0)),
            Some((f64::NAN, 6.0)),
            Some((-1.0, -0.5)),
            None,
            Some((5.5, 7.0)),
        ] {
            let t = c.next(batch, 10);
            assert!(t.start_ns > 0, "{batch:?}");
            assert!(t.start_ns <= t.end_ns, "{batch:?}");
            assert!(t.start_ns >= last, "{batch:?}");
            last = t.end_ns;
        }
    }
}
