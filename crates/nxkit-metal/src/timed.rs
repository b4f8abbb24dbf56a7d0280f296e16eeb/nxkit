//! Timed presents: the least time that a shown frame stays on screen. The host sets it when it plans the refresh, and
//! every present of a frame reads it, so it sits below the frames and the presenter.

/// The least time in microseconds that a shown frame stays before the next one may appear. 0 is off.
// Process-wide (W26): the host sets it from an AppKit callback (the screen changed), and a present on the encoder's
// thread reads it.
static MIN_STAY_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Turns on timed presents: every frame is presented with `presentAfterMinimumDuration:` of `secs`, so the display
/// keeps the frame before it on screen at least that long. A screen that adapts its refresh rate (ProMotion) then
/// shows each frame for exactly its period, whatever rate the display runs at in that moment. `None` turns it off.
/// Metal can refuse the call on a drawable of the display link. The first refusal turns timed presents off again.
pub fn set_timed_present(secs: Option<f64>) {
    let us = secs
        .filter(|s| s.is_finite() && *s > 0.0)
        .map_or(0, |s| (s * 1e6).round() as u64);
    MIN_STAY_US.store(us, std::sync::atomic::Ordering::Relaxed);
}

/// The minimum stay of timed presents in seconds, when they are on.
pub fn timed_present() -> Option<f64> {
    match MIN_STAY_US.load(std::sync::atomic::Ordering::Relaxed) {
        0 => None,
        us => Some(us as f64 / 1e6),
    }
}
