//! The ranges that transform feedback writes.
//!
//! A game binds a feedback buffer (`BindTransformFeedbackBuffer`) and captures the outputs of a vertex or geometry
//! stage into it, and a later draw reads that range as a vertex stream (`DrawTransformFeedback`, or a plain draw over
//! the same bytes). The host does not run that capture yet, so the bytes there are not the console's. The walker keeps
//! the ranges bound for feedback, so that a draw reading one is left out ([`crate::skip::Skip::Feedback`]) instead of
//! drawing what is left in the memory (Xenoblade Chronicles 3's cloth and hair spread over the screen as long spikes).
//!
//! A range counts for [`KEEP_SUBMITS`] submits after its last binding: memory that a game reuses for other vertex
//! data later is not held for good.

/// The submits that a range stays after it was last bound for feedback.
const KEEP_SUBMITS: u64 = 2048;
/// The most ranges kept. The oldest goes first.
const MAX_RANGES: usize = 1024;

/// The feedback ranges of recent submits: start, end (guest GPU addresses) and the submit they were last bound in.
#[derive(Default)]
pub(super) struct FeedbackRanges {
    ranges: Vec<(u64, u64, u64)>,
    submit: u64,
}

impl FeedbackRanges {
    /// A new submit begins: ranges not bound for a while go.
    pub(super) fn tick(&mut self) {
        self.submit += 1;
        let now = self.submit;
        self.ranges
            .retain(|r| now.saturating_sub(r.2) <= KEEP_SUBMITS);
    }

    /// `size` bytes at `addr` were bound for feedback.
    pub(super) fn note(&mut self, addr: u64, size: u64) {
        if addr == 0 || size == 0 {
            return;
        }
        let end = addr.saturating_add(size);
        if let Some(r) = self.ranges.iter_mut().find(|r| r.0 == addr && r.1 == end) {
            r.2 = self.submit;
            return;
        }
        if self.ranges.len() >= MAX_RANGES {
            self.ranges.remove(0);
        }
        self.ranges.push((addr, end, self.submit));
    }

    /// Whether `[addr, addr + size)` overlaps a feedback range.
    pub(super) fn reads(&self, addr: u64, size: u64) -> bool {
        let end = addr.saturating_add(size.max(1));
        addr != 0 && self.ranges.iter().any(|r| addr < r.1 && r.0 < end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stream_over_a_feedback_range_reads_it_until_the_range_ages_out() {
        let mut f = FeedbackRanges::default();
        f.tick();
        f.note(0x1000, 0x100);
        assert!(f.reads(0x1080, 0x40));
        assert!(f.reads(0x0f00, 0x101));
        assert!(!f.reads(0x1100, 0x40));
        assert!(!f.reads(0x0f00, 0x100));
        for _ in 0..KEEP_SUBMITS {
            f.tick();
        }
        assert!(f.reads(0x1000, 4), "still within the window");
        f.tick();
        assert!(!f.reads(0x1000, 4), "aged out");
    }

    #[test]
    fn binding_a_range_again_keeps_it() {
        let mut f = FeedbackRanges::default();
        f.note(0x1000, 0x100);
        for _ in 0..KEEP_SUBMITS {
            f.tick();
        }
        f.note(0x1000, 0x100);
        f.tick();
        assert!(f.reads(0x1000, 4));
        f.note(0, 0x100);
        f.note(0x2000, 0);
        assert!(!f.reads(0x2000, 4));
    }
}
