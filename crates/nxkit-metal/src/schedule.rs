//! The tick scheduler (R28).
//!
//! The display link calls the host once per tick. The scheduler decides what the tick shows, from the frames that are
//! ready. It has no clock and no Metal type, so tests run it with plain numbers.
//!
//! With [`FrameSource::Interpolate`] the display shows a game frame one tick after it is ready. The tick in between
//! shows the interpolated frame of the previous and the new game frame. That is one tick of added latency, which MetalFX
//! needs because it takes two frames.

use std::collections::VecDeque;

use crate::policy::FrameSource;

/// What a tick shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shown {
    /// A frame the game made.
    Game {
        /// The number of the frame.
        frame: u64,
    },
    /// The frame between two game frames.
    Interpolated {
        /// The earlier game frame.
        from: u64,
        /// The later game frame.
        to: u64,
    },
}

/// The decision of one tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TickAction {
    /// Present this at the target time of the tick.
    Present(Shown),
    /// Nothing is due. The display keeps the last frame.
    Hold,
    /// A frame was due and none was ready. The display keeps the last frame.
    Missed,
}

/// The scheduler.
#[derive(Debug)]
pub struct TickScheduler {
    source: FrameSource,
    ready: VecDeque<u64>,
    last_game: Option<u64>,
    second: Option<u64>,
    since_present: u32,
}

impl TickScheduler {
    /// A scheduler for a source of frames.
    pub fn new(source: FrameSource) -> Self {
        let since_present = match source {
            FrameSource::Hold { ticks_per_frame } => ticks_per_frame,
            _ => 0,
        };
        Self {
            source,
            ready: VecDeque::new(),
            last_game: None,
            second: None,
            since_present,
        }
    }

    /// Changes the source of frames, for a title that changes its present interval while it runs. A switch to
    /// [`FrameSource::Hold`] shows the next ready frame at once.
    pub fn set_source(&mut self, source: FrameSource) {
        if self.source == source {
            return;
        }
        self.source = source;
        if let FrameSource::Hold { ticks_per_frame } = source {
            self.since_present = ticks_per_frame;
        }
    }

    /// The game has a finished frame. Frames show in the order they were made.
    pub fn frame_ready(&mut self, frame: u64) {
        self.ready.push_back(frame);
    }

    /// The frames that are ready and not yet shown.
    pub fn queued(&self) -> usize {
        self.ready.len()
    }

    /// Decides what this tick shows.
    pub fn on_tick(&mut self) -> TickAction {
        match self.source {
            FrameSource::PerTick => match self.ready.pop_front() {
                Some(frame) => self.show_game(frame),
                None => self.miss(),
            },
            FrameSource::Hold { ticks_per_frame } => {
                self.since_present = self.since_present.saturating_add(1);
                if self.since_present < ticks_per_frame {
                    return TickAction::Hold;
                }
                match self.ready.pop_front() {
                    Some(frame) => {
                        self.since_present = 0;
                        self.show_game(frame)
                    }
                    None => self.miss(),
                }
            }
            FrameSource::Interpolate => {
                if let Some(frame) = self.second.take() {
                    return TickAction::Present(Shown::Game { frame });
                }
                match self.ready.pop_front() {
                    Some(to) => match self.last_game.replace(to) {
                        Some(from) => {
                            self.second = Some(to);
                            TickAction::Present(Shown::Interpolated { from, to })
                        }
                        None => TickAction::Present(Shown::Game { frame: to }),
                    },
                    None => self.miss(),
                }
            }
        }
    }

    fn show_game(&mut self, frame: u64) -> TickAction {
        self.last_game = Some(frame);
        TickAction::Present(Shown::Game { frame })
    }

    // Before the first frame nothing is due, so the display waiting for the game is not a miss.
    fn miss(&self) -> TickAction {
        if self.last_game.is_some() {
            TickAction::Missed
        } else {
            TickAction::Hold
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(frame: u64) -> TickAction {
        TickAction::Present(Shown::Game { frame })
    }

    #[test]
    fn per_tick_shows_each_frame_in_order() {
        let mut s = TickScheduler::new(FrameSource::PerTick);
        assert_eq!(s.on_tick(), TickAction::Hold);
        s.frame_ready(1);
        s.frame_ready(2);
        assert_eq!(s.on_tick(), game(1));
        assert_eq!(s.on_tick(), game(2));
        assert_eq!(s.on_tick(), TickAction::Missed);
    }

    #[test]
    fn hold_shows_a_frame_every_second_tick() {
        let mut s = TickScheduler::new(FrameSource::Hold { ticks_per_frame: 2 });
        s.frame_ready(1);
        assert_eq!(s.on_tick(), game(1));
        s.frame_ready(2);
        assert_eq!(s.on_tick(), TickAction::Hold);
        assert_eq!(s.on_tick(), game(2));
        assert_eq!(s.on_tick(), TickAction::Hold);
        assert_eq!(s.on_tick(), TickAction::Missed);
        s.frame_ready(3);
        assert_eq!(s.on_tick(), game(3));
    }

    #[test]
    fn interpolation_shows_the_middle_frame_first() {
        let mut s = TickScheduler::new(FrameSource::Interpolate);
        s.frame_ready(1);
        assert_eq!(s.on_tick(), game(1));
        s.frame_ready(2);
        assert_eq!(
            s.on_tick(),
            TickAction::Present(Shown::Interpolated { from: 1, to: 2 })
        );
        assert_eq!(s.on_tick(), game(2));
        s.frame_ready(3);
        assert_eq!(
            s.on_tick(),
            TickAction::Present(Shown::Interpolated { from: 2, to: 3 })
        );
        assert_eq!(s.on_tick(), game(3));
        assert_eq!(s.on_tick(), TickAction::Missed);
    }

    #[test]
    fn nothing_is_missed_before_the_first_frame() {
        let mut s = TickScheduler::new(FrameSource::Interpolate);
        assert_eq!(s.on_tick(), TickAction::Hold);
        assert_eq!(s.queued(), 0);
    }
}
