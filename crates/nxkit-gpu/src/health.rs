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
];

/// Counts what frame `n` left out and prints the report when it is due.
pub(crate) fn frame(n: u64, stats: &EncodeStats) {
    for (why, count) in SKIPS.iter().zip(stats.skipped) {
        note_n(*why, u64::from(count));
    }
    note_n(Reason::UploadScaledLost, u64::from(stats.scaled_upload_lost));
    frame_end(n);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_skip_has_the_reason_of_its_name() {
        let all = [
            Skip::Detail,
            Skip::Program,
            Skip::Binding,
            Skip::Primitive,
            Skip::Index,
            Skip::Format,
            Skip::Target,
            Skip::Cull,
            Skip::Texture,
            Skip::Pipeline,
            Skip::Buffer,
            Skip::Copy,
            Skip::Transfer,
            Skip::Encode,
            Skip::Pending,
        ];
        assert_eq!(all.len(), Skip::COUNT);
        for s in all {
            assert_eq!(SKIPS[s as usize].name(), format!("skip.{}", s.name()));
        }
    }
}
