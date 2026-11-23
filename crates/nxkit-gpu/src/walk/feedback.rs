//! The transform feedback state of the walk, and the ranges that captures the host cannot run wrote.
//!
//! A game binds feedback buffers (`BindTransformFeedbackBuffer`), begins a capture (`BeginTransformFeedback`) and
//! draws: the outputs of the vertex or geometry stage go into the buffers. A later draw reads those bytes as a vertex
//! stream (`DrawTransformFeedback`, or a plain draw over the same bytes), as a storage buffer or as a uniform buffer.
//! Each draw while a capture is on gets a [`CaptureDetail`]: the bound buffers, and whether the capture state starts
//! at zero (a begin), is loaded from a control block (a resume) or is saved into one after the draw (an end or a
//! pause). The encoder runs the capture (`crate::feedback`).
//!
//! A capture that the encoder cannot run (an indexed or strip draw) leaves its buffers stale. The walker keeps those
//! ranges, so that a draw reading one is left out ([`crate::skip::Skip::Feedback`]) instead of drawing what is left in
//! the memory: Xenoblade Chronicles 3 simulates cloth and hair with a geometry shader that writes feedback, and drawn
//! from stale bytes the cloth spreads over the screen as long spikes.
//!
//! Only a buffer that is bound while a draw captures counts. A game binds a small buffer of shared data when it is
//! done (16 bytes in Xenoblade Chronicles 3), and that binding writes nothing.
//!
//! A range counts for [`KEEP_SUBMITS`] submits after its last capture: memory that a game reuses for other data later
//! is not held for good.

use std::sync::Arc;

use crate::detail::CaptureDetail;

/// The submits that a range stays after its last capture.
const KEEP_SUBMITS: u64 = 2048;
/// The most ranges kept. The oldest goes first.
const MAX_RANGES: usize = 1024;
/// The feedback buffer bindings of NVN.
const BINDINGS: usize = 4;

/// The feedback state of the walk and the ranges that recent captures wrote: start, end (guest GPU addresses) and
/// the submit of the last capture.
#[derive(Clone, Default)]
pub(super) struct FeedbackRanges {
    ranges: Vec<(u64, u64, u64)>,
    submit: u64,
    /// The bound feedback buffers: address and size.
    bound: [(u64, u64); BINDINGS],
    /// A capture is on: between `BeginTransformFeedback` (or a resume) and its end (or a pause).
    active: bool,
    /// A begin came, and no draw since.
    begin: bool,
    /// A resume from this control block came, and no draw since.
    load: Option<u64>,
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

    /// `TransformFeedback` with its `arg` (0 begin, 1 end, 2 pause, 3 resume) and control block `ctl` (0 for none).
    /// Gives the control block that an end or a pause saves the state into.
    pub(super) fn control(&mut self, arg: u16, ctl: u64) -> Option<u64> {
        match arg {
            0 => {
                (self.active, self.begin, self.load) = (true, true, None);
                None
            }
            3 => {
                self.active = true;
                self.load = (ctl != 0).then_some(ctl);
                None
            }
            1 | 2 => {
                self.active = false;
                (ctl != 0).then_some(ctl)
            }
            _ => None,
        }
    }

    pub(super) fn active(&self) -> bool {
        self.active
    }

    /// A draw. While a capture is on it writes the bound buffers, and it gets the detail of its capture. A capture that
    /// the encoder cannot run (`runnable` false) keeps its ranges, so that their readers are left out.
    pub(super) fn draw(
        &mut self,
        geometry: Option<Arc<str>>,
        runnable: bool,
    ) -> Option<Box<CaptureDetail>> {
        if !self.active {
            return None;
        }
        if !runnable {
            for (addr, size) in self.bound {
                self.note(addr, size);
            }
        }
        Some(Box::new(CaptureDetail {
            buffers: self.bound,
            begin: std::mem::take(&mut self.begin),
            load: self.load.take(),
            save: None,
            geometry,
        }))
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
        f.control(0, 0);
        f.draw(None, false);
        f.control(1, 0);
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
        assert!(f.draw(None, false).is_none());
        f.control(0, 0);
        f.bind(1, 0x3000, 0x10);
        f.control(1, 0);
        assert!(f.draw(None, false).is_none());
        assert!(!f.reads(0x1000, 4), "no draw while the capture was on");
        assert!(!f.reads(0x3000, 4));
        // A pause stops the capture, a resume starts it again.
        f.control(0, 0);
        f.control(2, 0);
        f.draw(None, false);
        assert!(!f.reads(0x1000, 4));
        f.control(3, 0);
        f.draw(None, false);
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

    #[test]
    fn a_capture_draw_carries_its_buffers_and_its_state_steps() {
        let mut f = FeedbackRanges::default();
        f.bind(0, 0x1000, 0x100);
        assert_eq!(f.control(0, 0x900), None);
        let c = f.draw(None, true).expect("capture");
        assert_eq!(c.buffers[0], (0x1000, 0x100));
        assert!(c.begin && c.load.is_none() && c.save.is_none());
        assert!(
            !f.draw(None, true).expect("capture").begin,
            "only the first draw begins"
        );
        assert_eq!(f.control(2, 0x900), Some(0x900), "a pause saves");
        f.control(3, 0x900);
        let c = f.draw(None, true).expect("capture");
        assert_eq!((c.begin, c.load), (false, Some(0x900)));
        assert_eq!(
            f.control(1, 0),
            None,
            "an end without a control block saves nothing"
        );
        assert!(
            !f.reads(0x1000, 4),
            "a capture that runs leaves no stale range"
        );
    }
}
