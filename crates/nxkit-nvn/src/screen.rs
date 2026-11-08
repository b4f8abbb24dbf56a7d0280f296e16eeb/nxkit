//! What the screen showed of each present, and the GPU time of the latest finished frame.
//!
//! The backend writes them: the display reports when the drawable of a present went on screen, and a fence wait sees
//! the GPU time of the frame it waited for. The frame step reads them at the next presents. A game that waits on its own
//! GPU work every frame (Breath of the Wild) has that wait in the CPU time of its frames, so only what the screen shows
//! says whether a frame was late.
//!
//! It also counts how many refreshes each shown frame stayed (`NXKIT_DIAG=judder`): on a screen that shows a frame for
//! two refreshes every time, every shown frame is a 2x. A frame that stays 3 refreshes now and then is judder.

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
    let v = if secs.is_finite() && secs > 0.0 {
        secs
    } else {
        -1.0
    };
    let i = slot(present);
    SHOWN[i].store(v.to_bits(), Ordering::Relaxed);
    TAG[i].store(present, Ordering::Release);
    judder_note(v);
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
    if v >> 8 == present {
        (v & 0xFF) as u32
    } else {
        0
    }
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

/// The bins of the judder histogram: a frame stayed 1, 2, 3, 4 or 5 and more refreshes, and the frames the display
/// dropped.
const BINS: usize = 6;
/// The shown frames per `judder` line.
const JUDDER_EVERY: u64 = 120;

/// The refresh rate the screen shows, as `f64` bits. 0 is not known.
static REFRESH_HZ: AtomicU64 = AtomicU64::new(0);
/// The time of the frame shown before, as `f64` bits. 0 is none.
static PREV_SHOWN: AtomicU64 = AtomicU64::new(0);
static BIN: [AtomicU64; BINS] = [const { AtomicU64::new(0) }; BINS];
/// The sum of the refreshes that the counted frames stayed, in thousandths.
static STAY_MILLI: AtomicU64 = AtomicU64::new(0);

/// The refresh rate that a shown frame is measured against, in Hz. The host sets it from the screen (the rate of the
/// link on a screen that adapts its rate) and again when it changes.
pub fn set_refresh_hz(hz: f64) {
    if hz.is_finite() && hz > 0.0 {
        REFRESH_HZ.store(hz.to_bits(), Ordering::Relaxed);
        PREV_SHOWN.store(0, Ordering::Relaxed);
    }
}

/// The refreshes that a frame stayed when the next one came `gap_secs` later on a screen of `hz`, rounded to a whole
/// number and at least 1.
pub fn refreshes_held(gap_secs: f64, hz: f64) -> u32 {
    if !(gap_secs.is_finite() && hz.is_finite()) || gap_secs <= 0.0 || hz <= 0.0 {
        return 1;
    }
    (gap_secs * hz).round().clamp(1.0, 1000.0) as u32
}

/// The bin of a frame that stayed `held` refreshes.
fn bin(held: u32) -> usize {
    (held.max(1) as usize - 1).min(4)
}

fn judder_note(secs: f64) {
    let hz = f64::from_bits(REFRESH_HZ.load(Ordering::Relaxed));
    if hz <= 0.0 {
        return;
    }
    if secs <= 0.0 {
        BIN[5].fetch_add(1, Ordering::Relaxed);
        return;
    }
    let prev = f64::from_bits(PREV_SHOWN.swap(secs.to_bits(), Ordering::Relaxed));
    if prev <= 0.0 || secs <= prev {
        return;
    }
    let held = refreshes_held(secs - prev, hz);
    BIN[bin(held)].fetch_add(1, Ordering::Relaxed);
    STAY_MILLI.fetch_add(
        ((secs - prev) * hz * 1000.0).round() as u64,
        Ordering::Relaxed,
    );
    let counted: u64 = BIN[..5].iter().map(|b| b.load(Ordering::Relaxed)).sum();
    if counted >= JUDDER_EVERY {
        let b: Vec<u64> = BIN.iter().map(|b| b.swap(0, Ordering::Relaxed)).collect();
        let mean = STAY_MILLI.swap(0, Ordering::Relaxed) as f64 / 1000.0 / counted as f64;
        if crate::diag::on("judder") {
            eprintln!("{}", judder_line(hz, &b, mean));
        }
    }
}

/// The `judder` line: the refresh rate, how many frames stayed 1, 2, 3, 4 and 5 or more refreshes, the frames dropped
/// and the mean stay in refreshes. A steady game has one bin only. Two bins side by side (2x and 3x) are judder.
pub fn judder_line(hz: f64, bins: &[u64], mean: f64) -> String {
    let get = |i: usize| bins.get(i).copied().unwrap_or(0);
    format!(
        "DIAG judder refresh {hz:.2} Hz: 1x {} 2x {} 3x {} 4x {} 5x+ {} dropped {} mean {mean:.2} refreshes",
        get(0),
        get(1),
        get(2),
        get(3),
        get(4),
        get(5)
    )
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

    #[test]
    fn a_frame_is_held_for_its_refreshes() {
        assert_eq!(refreshes_held(1.0 / 60.0, 120.0), 2);
        assert_eq!(refreshes_held(1.0 / 60.0, 144.0), 2);
        assert_eq!(refreshes_held(2.6 / 144.0, 144.0), 3);
        assert_eq!(refreshes_held(0.0, 120.0), 1);
        assert_eq!(refreshes_held(f64::NAN, 120.0), 1);
        assert_eq!((bin(1), bin(2), bin(5), bin(9)), (0, 1, 4, 4));
    }

    #[test]
    fn the_judder_line_names_every_bin() {
        let l = judder_line(120.0, &[0, 118, 2, 0, 0, 1], 2.02);
        assert_eq!(
            l,
            "DIAG judder refresh 120.00 Hz: 1x 0 2x 118 3x 2 4x 0 5x+ 0 dropped 1 mean 2.02 refreshes"
        );
    }
}
