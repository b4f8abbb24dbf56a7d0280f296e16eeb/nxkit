//! The encoder side of the render health report (`NXKIT_DIAG=health`, see `nxkit_nvn::diag` and
//! `docs/diagnostics.md`). The causes are counted where they happen. This adds the skip counters of each frame and
//! ends the frame of the report.

use nxkit_nvn::diag::{Reason, frame_end, note_n};

use crate::stats::{EncodeStats, Skip};

/// The reason of the report for each [`Skip`], indexed by `Skip as usize`.
const SKIPS: [Reason; Skip::COUNT] = [
    Reason::SkipDetail,
    Reason::SkipProgram,
    Reason::SkipBinding,
    Reason::SkipPrimitive,
    Reason::SkipIndex,
    Reason::SkipFormat,
    Reason::SkipTarget,
    Reason::SkipCull,
    Reason::SkipTexture,
    Reason::SkipPipeline,
    Reason::SkipBuffer,
    Reason::SkipCopy,
    Reason::SkipTransfer,
    Reason::SkipEncode,
    Reason::SkipPending,
    Reason::SkipListed,
];

/// Counts what frame `n` left out and prints the report when it is due.
pub(crate) fn frame(n: u64, stats: &EncodeStats) {
    for (why, count) in SKIPS.iter().zip(stats.skipped) {
        note_n(*why, u64::from(count));
    }
    note_n(
        Reason::UploadScaledLost,
        u64::from(stats.scaled_upload_lost),
    );
    note_n(Reason::UploadScaled, u64::from(stats.scaled_uploads));
    native_report(n);
    frame_end(n);
}

/// Prints the native path's counters (`ndve.native`, `ndve.fallback`) with the report: every `REPORT_EVERY` frames and
/// at once when a counter shows up that the last report did not have. They are not in `Reason` (that list is in
/// `nxkit-nvn`, which knows no network), so they print as a block of their own.
fn native_report(n: u64) {
    if !nxkit_nvn::diag::health_on() {
        return;
    }
    let (lines, fresh) = crate::native::health_lines();
    if !lines.is_empty() && (fresh || n.is_multiple_of(nxkit_nvn::diag::REPORT_EVERY)) {
        eprint!("health: native paths (frame {n})\n{lines}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_skip_has_the_reason_of_its_name() {
        for (i, s) in Skip::ALL.into_iter().enumerate() {
            assert_eq!(s as usize, i);
            assert_eq!(SKIPS[s as usize].name(), format!("skip.{}", s.name()));
        }
    }
}
