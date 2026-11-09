//! How the game's dispatches are matched to decoded pictures, per video stream: the counts for the diagnostic line and
//! the back-off that makes a mismatch cost nothing.
//!
//! A dispatch names its picture in one of three ways ([`How`]): its planes are the very memory the game read the
//! picture into (`address`), a copy the game recorded leads from them to that memory (`copy`), or the picture's
//! fingerprint was found in them (`fingerprint`). When [`GIVE_UP`] dispatches in a row match none of them, the feeder
//! stops converting and predicting ([`Matcher::off`]) until the next stream starts ([`Matcher::restart`]).

/// The consecutive dispatches without a match after which the stream is given up.
pub(super) const GIVE_UP: u32 = 30;

/// The dispatches after which a stream prints its first line, so a run that never ends still says something.
const FIRST_REPORT: u32 = 60;

/// How a dispatch's picture was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum How {
    /// The dispatch's planes are the memory of the read.
    Address,
    /// A copy leads from the dispatch's planes to the memory of the read.
    Copy,
    /// The fingerprint of the picture lies in the dispatch's planes (or in the memory a copy read from).
    Print,
}

/// The matches of one stream.
#[derive(Debug, Default)]
pub(super) struct Matcher {
    stream: u32,
    by: [u32; 3],
    missed: u32,
    /// The misses since the last match.
    run: u32,
    off: bool,
}

impl Matcher {
    /// True when the stream is given up: nothing is converted and every dispatch is a miss.
    pub(super) fn off(&self) -> bool {
        self.off
    }

    fn total(&self) -> u32 {
        self.by.iter().sum::<u32>() + self.missed
    }

    /// The line of the stream so far.
    pub(super) fn line(&self) -> String {
        let hits: u32 = self.by.iter().sum();
        let total = self.total().max(1);
        format!(
            "nxkit: ndve.ane: stream {}: {} of {} dispatches matched a picture ({:.0}%): {} by address, {} by copy, {} by fingerprint{}",
            self.stream,
            hits,
            self.total(),
            f64::from(hits) * 100.0 / f64::from(total),
            self.by[0],
            self.by[1],
            self.by[2],
            if self.off {
                "; given up until the next stream"
            } else {
                ""
            }
        )
    }

    /// A dispatch found its picture. Returns a line to print when there is one.
    pub(super) fn hit(&mut self, how: How) -> Option<String> {
        if self.off {
            return None;
        }
        let first = self.by[how as usize] == 0;
        self.by[how as usize] += 1;
        self.run = 0;
        (first || self.total() == FIRST_REPORT).then(|| self.line())
    }

    /// A dispatch found no picture. Returns a line to print when the stream is given up (or at the first report).
    pub(super) fn miss(&mut self) -> Option<String> {
        if self.off {
            return None;
        }
        self.missed += 1;
        self.run += 1;
        if self.run >= GIVE_UP {
            self.off = true;
            return Some(self.line());
        }
        (self.total() == FIRST_REPORT).then(|| self.line())
    }

    /// A stream starts: the counts begin again and the stream is tried again. Returns the line of the stream that
    /// ended, when it had any dispatches.
    pub(super) fn restart(&mut self) -> Option<String> {
        let done = (self.total() > 0 && !self.off).then(|| self.line());
        *self = Self {
            stream: self.stream + 1,
            ..Self::default()
        };
        done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thirty_misses_in_a_row_give_the_stream_up() {
        let mut m = Matcher::default();
        for i in 1..GIVE_UP {
            assert!(m.miss().is_none() || i == FIRST_REPORT, "miss {i}");
            assert!(!m.off());
        }
        let line = m.miss().unwrap_or_default();
        assert!(m.off());
        assert!(line.contains("0 of 30"), "{line}");
        assert!(line.contains("given up"), "{line}");
        // Nothing more is counted while it is off.
        assert!(m.miss().is_none());
        assert!(m.hit(How::Copy).is_none());
        assert_eq!(m.total(), GIVE_UP);
    }

    #[test]
    fn a_hit_clears_the_run_of_misses() {
        let mut m = Matcher::default();
        for _ in 0..GIVE_UP - 1 {
            let _ = m.miss();
        }
        let _ = m.hit(How::Print);
        for _ in 0..GIVE_UP - 1 {
            let _ = m.miss();
        }
        assert!(!m.off());
        let _ = m.miss();
        assert!(m.off());
    }

    #[test]
    fn the_next_stream_tries_again_and_reports_the_one_before() {
        let mut m = Matcher::default();
        let first = m.hit(How::Address).unwrap_or_default();
        assert!(
            first.contains("stream 0") && first.contains("1 by address"),
            "{first}"
        );
        let _ = m.hit(How::Copy);
        let _ = m.hit(How::Copy);
        let _ = m.miss();
        let done = m.restart().unwrap_or_default();
        assert!(done.contains("3 of 4"), "{done}");
        assert!(
            done.contains("(75%)") && done.contains("2 by copy"),
            "{done}"
        );
        assert!(!m.off());
        assert!(m.restart().is_none());
        let line = m.hit(How::Print).unwrap_or_default();
        assert!(
            line.contains("stream 2") && line.contains("1 by fingerprint"),
            "{line}"
        );
    }

    #[test]
    fn a_given_up_stream_is_tried_again_after_a_restart() {
        let mut m = Matcher::default();
        for _ in 0..GIVE_UP {
            let _ = m.miss();
        }
        assert!(m.off());
        assert!(m.restart().is_none());
        assert!(!m.off());
        assert!(m.miss().is_none());
    }
}
