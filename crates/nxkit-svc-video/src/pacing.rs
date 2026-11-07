//! Whether a movie is playing. The pacing service holds the frame rate of the game while it is.
//!
//! A game that plays a 30 fps movie draws at the rate of the movie. The video service tells when a picture went to the
//! game within the last half second.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How long after the last picture a movie still counts as playing.
const WINDOW: Duration = Duration::from_millis(500);

static PLAYING_UNTIL: AtomicU64 = AtomicU64::new(0);

fn now_ns() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    // One is added so that the first instant is not zero, which means "never".
    START.get_or_init(Instant::now).elapsed().as_nanos() as u64 + 1
}

/// A picture went to the game.
pub(crate) fn picture() {
    PLAYING_UNTIL.store(now_ns() + WINDOW.as_nanos() as u64, Ordering::Relaxed);
}

/// The movie ended.
pub(crate) fn stop() {
    PLAYING_UNTIL.store(0, Ordering::Relaxed);
}

/// Whether a picture went to the game within the last half second.
pub fn playing() -> bool {
    PLAYING_UNTIL.load(Ordering::Relaxed) > now_ns()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_starts_it_and_a_stop_ends_it() {
        stop();
        assert!(!playing());
        picture();
        assert!(playing());
        stop();
        assert!(!playing());
    }
}
