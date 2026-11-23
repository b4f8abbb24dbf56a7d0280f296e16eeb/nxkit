//! The ranges that transform feedback writes.
//!
//! A game binds feedback buffers (`BindTransformFeedbackBuffer`), begins a capture (`BeginTransformFeedback`) and
//! draws: the outputs of the vertex or geometry stage go into the buffers. A later draw reads those bytes as a vertex
//! stream (`DrawTransformFeedback`, or a plain draw over the same bytes), as a storage buffer or as a uniform buffer.
//! The host does not run the capture yet, so the bytes there are not the console's. The walker keeps the ranges that a
//! capture wrote, so that a draw reading one is left out ([`crate::skip::Skip::Feedback`]) instead of drawing what is
//! left in the memory: Xenoblade Chronicles 3 simulates cloth and hair with a geometry shader that writes feedback,
//! and the draws of the characters read the result as a storage buffer. Drawn from stale bytes, the cloth spreads over
//! the screen as long spikes.
//!
//! Only a buffer that is bound while a draw captures counts. A game binds a small buffer of shared data when it is
//! done (16 bytes in Xenoblade Chronicles 3), and that binding writes nothing.
//!
//! A range counts for [`KEEP_SUBMITS`] submits after its last capture: memory that a game reuses for other data later
//! is not held for good.

/// The submits that a range stays after its last capture.
const KEEP_SUBMITS: u64 = 2048;
/// The most ranges kept. The oldest goes first.
const MAX_RANGES: usize = 1024;
/// The feedback buffer bindings of NVN.
const BINDINGS: usize = 4;

/// The feedback state of the walk and the ranges that recent captures wrote: start, end (guest GPU addresses) and
/// the submit of the last capture.
#[derive(Default)]
pub(super) struct FeedbackRanges {
    ranges: Vec<(u64, u64, u64)>,
    submit: u64,
    /// The bound feedback buffers: address and size.
    bound: [(u64, u64); BINDINGS],
    /// A capture is on: between `BeginTransformFeedback` (or a resume) and its end (or a pause).
    active: bool,
}

impl FeedbackRanges {
    /// A new submit begins: ranges not captured for a while go.
    pub(super) fn tick(&mut self) {
        self.submit += 1;
        let now = self.submit;
        self.ranges
            .retain(|r| now.saturating_sub(r.2) <= KEEP_SUBMITS);
    }

    /// `BindTransformFeedbackBuffer`: `size` bytes at `addr` for binding `index`.
    pub(super) fn bind(&mut self, index: u16, addr: u64, size: u64) {
        if let Some(b) = self.bound.get_mut(usize::from(index)) {
            *b = (addr, size);
        }
    }

    /// `TransformFeedback` with its `arg`: 0 begins and 3 resumes a capture, 1 ends and 2 pauses it.
    pub(super) fn control(&mut self, arg: u16) {
        match arg {
            0 | 3 => self.active = true,
            1 | 2 => self.active = false,
            _ => {}
        }
    }

    /// A draw: while a capture is on, it writes the bound buffers.
    pub(super) fn draw(&mut self) {
        if self.active {
            for (addr, size) in self.bound {
                self.note(addr, size);
            }
        }
    }

    fn note(&mut self, addr: u64, size: u64) {
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

    /// Whether `[addr, addr + size)` overlaps a range that a capture wrote.
    pub(super) fn reads(&self, addr: u64, size: u64) -> bool {
        let end = addr.saturating_add(size.max(1));
        addr != 0 && self.ranges.iter().any(|r| addr < r.1 && r.0 < end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn captured(f: &mut FeedbackRanges, addr: u64, size: u64) {
        f.bind(0, addr, size);
        f.control(0);
        f.draw();
        f.control(1);
    }

    #[test]
    fn a_read_of_a_captured_range_counts_until_the_range_ages_out() {
        let mut f = FeedbackRanges::default();
        f.tick();
        captured(&mut f, 0x1000, 0x100);
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
    fn a_buffer_bound_without_a_capture_does_not_count() {
        let mut f = FeedbackRanges::default();
        f.bind(0, 0x1000, 0x100);
        f.draw();
        f.control(0);
        f.bind(1, 0x3000, 0x10);
        f.control(1);
        f.draw();
        assert!(!f.reads(0x1000, 4), "no draw while the capture was on");
        assert!(!f.reads(0x3000, 4));
        // A pause stops the capture, a resume starts it again.
        f.control(0);
        f.control(2);
        f.draw();
        assert!(!f.reads(0x1000, 4));
        f.control(3);
        f.draw();
        assert!(f.reads(0x1000, 4) && f.reads(0x3000, 4));
    }

    #[test]
    fn capturing_a_range_again_keeps_it() {
        let mut f = FeedbackRanges::default();
        captured(&mut f, 0x1000, 0x100);
        for _ in 0..KEEP_SUBMITS {
            f.tick();
        }
        captured(&mut f, 0x1000, 0x100);
        f.tick();
        assert!(f.reads(0x1000, 4));
        captured(&mut f, 0, 0x100);
        captured(&mut f, 0x2000, 0);
        assert!(!f.reads(0x2000, 4));
        f.bind(9, 0x5000, 0x10);
    }
}
