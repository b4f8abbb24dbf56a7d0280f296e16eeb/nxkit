//! Timed presents: the least time that a shown frame stays on screen. The host's refresh plan sets it, and every
//! present of a frame ring that has it reads it.

use std::sync::atomic::{AtomicU64, Ordering};

/// The least time that a shown frame stays before the next one may appear. The host makes one per run and gives it to
/// the refresh plan, which sets it, and to the frame ring of the present, which reads it (W26).
#[derive(Debug, Default)]
pub struct TimedPresent {
    /// In microseconds. 0 is off.
    min_stay_us: AtomicU64,
}

impl TimedPresent {
    /// Turns on timed presents: every frame is presented with `presentAfterMinimumDuration:` of `secs`, so the display
    /// keeps the frame before it on screen at least that long. A screen that adapts its refresh rate (ProMotion) then
    /// shows each frame for exactly its period, whatever rate the display runs at in that moment. `None` turns it off.
    /// Metal can refuse the call on a drawable of the display link. The first refusal turns timed presents off again.
    pub fn set(&self, secs: Option<f64>) {
        let us = secs
            .filter(|s| s.is_finite() && *s > 0.0)
            .map_or(0, |s| (s * 1e6).round() as u64);
        self.min_stay_us.store(us, Ordering::Relaxed);
    }

    /// The minimum stay of timed presents in seconds, when they are on.
    pub fn get(&self) -> Option<f64> {
        match self.min_stay_us.load(Ordering::Relaxed) {
            0 => None,
            us => Some(us as f64 / 1e6),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stay_rounds_to_microseconds_and_none_turns_it_off() {
        let t = TimedPresent::default();
        assert_eq!(t.get(), None);
        t.set(Some(1.0 / 60.0));
        assert_eq!(t.get(), Some(0.016667));
        t.set(Some(f64::NAN));
        assert_eq!(t.get(), None);
    }
}
