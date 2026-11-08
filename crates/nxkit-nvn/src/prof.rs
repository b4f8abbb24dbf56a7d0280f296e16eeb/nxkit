//! `NXKIT_DIAG=decodeprof`: where the time of a submit goes.
//!
//! The decoder counts lookups, hits, misses and the depth of the recursion, and times the parts of a submit. The
//! front end prints the mean per submit every [`REPORT_EVERY`] submits and starts again. The counters are plain adds.
//! The clocks run only with the category on: each reads the time twice, so the numbers include that cost.

use std::sync::OnceLock;
use std::time::Instant;

/// The submits between two reports.
pub const REPORT_EVERY: u64 = 300;

/// Whether `NXKIT_DIAG` names `decodeprof` (read once).
pub fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| crate::diag::on("decodeprof"))
}

/// A stopwatch that does nothing when the category is off.
#[derive(Clone, Copy, Debug)]
pub struct Clock(Option<Instant>);

impl Clock {
    /// Starts the stopwatch.
    pub fn start() -> Self {
        Self(on().then(Instant::now))
    }

    /// The nanoseconds since the start, zero when the category is off.
    pub fn ns(self) -> u64 {
        self.0.map_or(0, |t| t.elapsed().as_nanos() as u64)
    }
}

/// The counters and clocks of the decode path since the last report.
#[derive(Clone, Copy, Debug, Default)]
pub struct DecodeProf {
    /// Submits of one recording.
    pub submits: u64,
    /// Calls of `summary`, for the submitted recording and for every call record under it.
    pub lookups: u64,
    /// Lookups that the cache answered.
    pub hits: u64,
    /// Lookups that decoded a recording.
    pub misses: u64,
    /// The sum of the call depths of all lookups, and the deepest one.
    pub depth_sum: u64,
    /// The deepest call depth of a lookup.
    pub depth_max: u32,
    /// Summaries dropped by a lookup because a resolution changed.
    pub dropped: u64,
    /// Records read by the decoder.
    pub records: u64,
    /// Commands that a submit appended to the frame.
    pub cmds: u64,
    /// Resolutions that a hit checked one by one, and hits that the epoch check let skip them.
    pub refs_checked: u64,
    /// Hits that skipped the check of their addresses and handles because nothing changed.
    pub epoch_skips: u64,
    /// Time of the cache lookup (`RecCache::find`), with the check of the resolutions inside it.
    pub ns_find: u64,
    /// Time of the check of the resolutions of hits, also in `ns_find`.
    pub ns_refs: u64,
    /// Time of a miss at the top: decode of the recording with everything below it.
    pub ns_miss: u64,
    /// Time of the records of misses (`Builder::apply`, including the read of the record), also in `ns_miss`.
    pub ns_apply: u64,
    /// Time of the splice of called summaries into their caller, also in `ns_miss`.
    pub ns_splice: u64,
    /// Time of the end of a decode: delta, key, hash of the references, also in `ns_miss`.
    pub ns_finish: u64,
    /// Time of the append of the summary to the frame and of the state update.
    pub ns_append: u64,
}

impl DecodeProf {
    /// One line with the mean per submit.
    pub fn describe(&self) -> String {
        let n = self.submits.max(1) as f64;
        let ms = |v: u64| v as f64 / n / 1e6;
        let per = |v: u64| v as f64 / n;
        format!(
            "decodeprof per submit: lookups {:.1} (hits {:.1}, misses {:.1}, dropped {:.2}), depth mean {:.2} max {}, \
             records {:.1}, cmds appended {:.0}, refs checked {:.0}, epoch skips {:.1} | \
             find {:.3} ms (refs {:.3}), miss decode {:.3} ms (records {:.3}, splice {:.3}, finish {:.3}), append {:.3} ms",
            per(self.lookups),
            per(self.hits),
            per(self.misses),
            per(self.dropped),
            self.depth_sum as f64 / self.lookups.max(1) as f64,
            self.depth_max,
            per(self.records),
            per(self.cmds),
            per(self.refs_checked),
            per(self.epoch_skips),
            ms(self.ns_find),
            ms(self.ns_refs),
            ms(self.ns_miss),
            ms(self.ns_apply),
            ms(self.ns_splice),
            ms(self.ns_finish),
            ms(self.ns_append),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_of_nothing_does_not_divide_by_zero() {
        assert!(DecodeProf::default().describe().contains("lookups 0.0"));
    }
}
