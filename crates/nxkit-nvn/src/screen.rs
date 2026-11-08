//! What the screen showed of each present, and the GPU time of the latest finished frame.
//!
//! The backend writes them: the display reports when the drawable of a present went on screen, and a fence wait sees
//! the GPU time of the frame it waited for. The frame step reads them at the next presents. A game that waits on its own
//! GPU work every frame (Breath of the Wild) has that wait in the CPU time of its frames, so only what the screen shows
//! says whether a frame was late.

use std::sync::atomic::{AtomicU64, Ordering};

/// The presents that the ring remembers. The frame step reads the one 8 presents back.
const RING: usize = 32;

static SHOWN: [AtomicU64; RING] = [const { AtomicU64::new(0) }; RING];
static TAG: [AtomicU64; RING] = [const { AtomicU64::new(0) }; RING];
static HOLD: [AtomicU64; RING] = [const { AtomicU64::new(0) }; RING];
/// The GPU time in milliseconds as `f64` bits. `u64::MAX` is none yet.
static GPU_MS: AtomicU64 = AtomicU64::new(u64::MAX);

fn slot(present: u64) -> usize {
    (present % RING as u64) as usize
}

/// Present `present` (counted from 1) went on screen at `secs` on the host clock. A time of 0 or less means the display
/// dropped it, and a present that was replaced before a tick showed it is dropped too.
pub fn shown(present: u64, secs: f64) {
    let v = if secs.is_finite() && secs > 0.0 { secs } else { -1.0 };
    let i = slot(present);
    SHOWN[i].store(v.to_bits(), Ordering::Relaxed);
    TAG[i].store(present, Ordering::Release);
}

/// When present `present` went on screen: negative when it was dropped, 0 when not known (yet).
pub fn shown_at(present: u64) -> f64 {
    let i = slot(present);
    if present == 0 || TAG[i].load(Ordering::Acquire) != present {
        return 0.0;
    }
    f64::from_bits(SHOWN[i].load(Ordering::Relaxed))
}

/// The frame step gave present `present` a hold of `vsyncs`.
pub fn set_hold(present: u64, vsyncs: u32) {
    HOLD[slot(present)].store(present << 8 | u64::from(vsyncs.min(255)), Ordering::Relaxed);
}

/// The hold of present `present`, 0 when not known.
pub fn hold(present: u64) -> u32 {
    let v = HOLD[slot(present)].load(Ordering::Relaxed);
    if v >> 8 == present { (v & 0xFF) as u32 } else { 0 }
}

/// The GPU took `ms` for the latest finished frame.
pub fn set_gpu_ms(ms: f64) {
    if ms.is_finite() && ms >= 0.0 {
        GPU_MS.store(ms.to_bits(), Ordering::Relaxed);
    }
}

/// The GPU time of the latest finished frame, when one finished.
pub fn gpu_ms() -> Option<f64> {
    let v = GPU_MS.load(Ordering::Relaxed);
    (v != u64::MAX).then(|| f64::from_bits(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_present_is_known_until_the_ring_comes_round() {
        shown(1001, 5.5);
        shown(1002, 0.0);
        assert_eq!(shown_at(1001), 5.5);
        assert_eq!(shown_at(1002), -1.0);
        assert_eq!(shown_at(1001 + RING as u64), 0.0);
        set_hold(1001, 3);
        assert_eq!(hold(1001), 3);
        assert_eq!(hold(1001 + RING as u64), 0);
    }
}
