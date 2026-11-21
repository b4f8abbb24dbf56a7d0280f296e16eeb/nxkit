//! What the screen showed of each present, and the GPU time of the latest finished frame.
//!
//! The backend writes them: the display reports when the drawable of a present went on screen, and a fence wait sees
//! the GPU time of the frame it waited for. The frame step reads them at the next presents. A game that waits on its own
//! GPU work every frame (Breath of the Wild) has that wait in the CPU time of its frames, so only what the screen shows
//! says whether a frame was late.
//!
//! The host makes one [`Screen`] for the run and hands it to the backend, the NVN context and the refresh (W26). Before,
//! these were process-wide statics.
//!
//! It also counts how many refreshes each shown frame stayed (`NXKIT_DIAG=judder`): on a screen that shows a frame for
//! two refreshes every time, every shown frame is a 2x. A frame that stays 3 refreshes now and then is judder.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

/// The presents that the ring remembers. The frame step reads the one 8 presents back.
const RING: usize = 32;

/// The presents per `NXKIT_PACE_DIAG` line of the interval between shown presents.
const PACE_EVERY: u32 = 120;

/// The bins of the judder histogram: a frame stayed 1, 2, 3, 4 or 5 and more refreshes, and the frames the display
/// dropped.
const BINS: usize = 6;
/// The shown frames per `judder` line.
const JUDDER_EVERY: u64 = 120;

/// What the screen showed of the latest presents, the GPU time of the latest finished frame and the judder count.
pub struct Screen {
    shown: [AtomicU64; RING],
    tag: [AtomicU64; RING],
    hold: [AtomicU64; RING],
    /// The GPU time in milliseconds as `f64` bits. `u64::MAX` is none yet.
    gpu_ms: AtomicU64,
    /// The function that [`Screen::shown`] calls for every present the display showed.
    shown_hook: Option<fn(u64, f64)>,
    pace: Mutex<PaceSpread>,
    /// The refresh rate the screen shows, as `f64` bits. 0 is not known.
    refresh_hz: AtomicU64,
    /// The refresh rate of the current mode of the screen, rounded to Hz, for the frame step.
    mode_hz: AtomicU32,
    /// The time of the frame shown before, as `f64` bits. 0 is none.
    prev_shown: AtomicU64,
    bin: [AtomicU64; BINS],
    /// The sum of the refreshes that the counted frames stayed, in thousandths.
    stay_milli: AtomicU64,
}

impl std::fmt::Debug for Screen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Screen")
            .field("gpu_ms", &self.gpu_ms())
            .field(
                "refresh_hz",
                &f64::from_bits(self.refresh_hz.load(Ordering::Relaxed)),
            )
            .finish()
    }
}

impl Default for Screen {
    fn default() -> Self {
        Screen::new(None)
    }
}

impl Screen {
    /// An empty record. `shown_hook` learns of every shown present: the host feeds the time of the display to the
    /// variable frame step with it. It runs on a Metal thread and must be short.
    pub fn new(shown_hook: Option<fn(u64, f64)>) -> Self {
        Screen {
            shown: [const { AtomicU64::new(0) }; RING],
            tag: [const { AtomicU64::new(0) }; RING],
            hold: [const { AtomicU64::new(0) }; RING],
            gpu_ms: AtomicU64::new(u64::MAX),
            shown_hook,
            pace: Mutex::new(PaceSpread::new()),
            refresh_hz: AtomicU64::new(0),
            mode_hz: AtomicU32::new(120),
            prev_shown: AtomicU64::new(0),
            bin: [const { AtomicU64::new(0) }; BINS],
            stay_milli: AtomicU64::new(0),
        }
    }

    /// Present `present` (counted from 1) went on screen at `secs` on the host clock. A time of 0 or less means the
    /// display dropped it, and a present that was replaced before a tick showed it is dropped too.
    pub fn shown(&self, present: u64, secs: f64) {
        let v = if secs.is_finite() && secs > 0.0 {
            secs
        } else {
            -1.0
        };
        let i = slot(present);
        self.shown[i].store(v.to_bits(), Ordering::Relaxed);
        self.tag[i].store(present, Ordering::Release);
        self.judder_note(v);
        self.pace_note(v);
        if v > 0.0
            && let Some(hook) = self.shown_hook
        {
            hook(present, v);
        }
    }

    fn pace_note(&self, secs: f64) {
        if secs <= 0.0 || !crate::settings::settings().pace_diag {
            return;
        }
        let line = self
            .pace
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .note(secs);
        if let Some(line) = line {
            eprintln!("{line}");
        }
    }

    /// When present `present` went on screen: negative when it was dropped, 0 when not known (yet).
    pub fn shown_at(&self, present: u64) -> f64 {
        let i = slot(present);
        if present == 0 || self.tag[i].load(Ordering::Acquire) != present {
            return 0.0;
        }
        f64::from_bits(self.shown[i].load(Ordering::Relaxed))
    }

    /// The frame step gave present `present` a hold of `vsyncs`.
    pub fn set_hold(&self, present: u64, vsyncs: u32) {
        self.hold[slot(present)]
            .store(present << 8 | u64::from(vsyncs.min(255)), Ordering::Relaxed);
    }

    /// The hold of present `present`, 0 when not known.
    pub fn hold(&self, present: u64) -> u32 {
        let v = self.hold[slot(present)].load(Ordering::Relaxed);
        if v >> 8 == present {
            (v & 0xFF) as u32
        } else {
            0
        }
    }

    /// The GPU took `ms` for the latest finished frame.
    pub fn set_gpu_ms(&self, ms: f64) {
        if ms.is_finite() && ms >= 0.0 {
            self.gpu_ms.store(ms.to_bits(), Ordering::Relaxed);
        }
    }

    /// The GPU time of the latest finished frame, when one finished.
    pub fn gpu_ms(&self) -> Option<f64> {
        let v = self.gpu_ms.load(Ordering::Relaxed);
        (v != u64::MAX).then(|| f64::from_bits(v))
    }

    /// The refresh rate that a shown frame is measured against, in Hz. The host sets it from the screen (the rate of
    /// the link on a screen that adapts its rate) and again when it changes.
    pub fn set_refresh_hz(&self, hz: f64) {
        if hz.is_finite() && hz > 0.0 {
            self.refresh_hz.store(hz.to_bits(), Ordering::Relaxed);
            self.prev_shown.store(0, Ordering::Relaxed);
        }
    }

    /// Sets the refresh rate of the current mode of the screen. A rate that is not a multiple of the step rate (144 Hz)
    /// shows a frame on time one refresh longer now and then, which the frame step must not take for lateness. Zero
    /// keeps the rate before.
    pub fn set_mode_hz(&self, hz: u32) {
        if hz > 0 {
            self.mode_hz.store(hz, Ordering::Relaxed);
        }
    }

    /// The refresh rate of the current mode of the screen, 120 until the host sets it.
    pub fn mode_hz(&self) -> u32 {
        self.mode_hz.load(Ordering::Relaxed)
    }

    fn judder_note(&self, secs: f64) {
        let hz = f64::from_bits(self.refresh_hz.load(Ordering::Relaxed));
        if hz <= 0.0 {
            return;
        }
        if secs <= 0.0 {
            self.bin[5].fetch_add(1, Ordering::Relaxed);
            return;
        }
        let prev = f64::from_bits(self.prev_shown.swap(secs.to_bits(), Ordering::Relaxed));
        if prev <= 0.0 || secs <= prev {
            return;
        }
        let held = refreshes_held(secs - prev, hz);
        self.bin[bin(held)].fetch_add(1, Ordering::Relaxed);
        self.stay_milli.fetch_add(
            ((secs - prev) * hz * 1000.0).round() as u64,
            Ordering::Relaxed,
        );
        let counted: u64 = self.bin[..5]
            .iter()
            .map(|b| b.load(Ordering::Relaxed))
            .sum();
        if counted >= JUDDER_EVERY {
            let b: Vec<u64> = self
                .bin
                .iter()
                .map(|b| b.swap(0, Ordering::Relaxed))
                .collect();
            let mean = self.stay_milli.swap(0, Ordering::Relaxed) as f64 / 1000.0 / counted as f64;
            if crate::diag::on("judder") {
                eprintln!("{}", judder_line(hz, &b, mean));
            }
        }
    }
}

fn slot(present: u64) -> usize {
    (present % RING as u64) as usize
}

/// The spread of the intervals between shown presents, for `NXKIT_PACE_DIAG=1`.
#[derive(Debug)]
struct PaceSpread {
    last: f64,
    n: u32,
    sum: f64,
    sum_sq: f64,
    min: f64,
    max: f64,
}

impl PaceSpread {
    const fn new() -> Self {
        Self {
            last: 0.0,
            n: 0,
            sum: 0.0,
            sum_sq: 0.0,
            min: f64::MAX,
            max: 0.0,
        }
    }

    /// A present was shown at `secs`. Returns the line for the last [`PACE_EVERY`] intervals when they are complete.
    fn note(&mut self, secs: f64) -> Option<String> {
        let prev = std::mem::replace(&mut self.last, secs);
        if prev <= 0.0 || secs <= prev {
            return None;
        }
        let ms = (secs - prev) * 1e3;
        self.n += 1;
        self.sum += ms;
        self.sum_sq += ms * ms;
        self.min = self.min.min(ms);
        self.max = self.max.max(ms);
        if self.n < PACE_EVERY {
            return None;
        }
        let n = f64::from(self.n);
        let mean = self.sum / n;
        let sd = (self.sum_sq / n - mean * mean).max(0.0).sqrt();
        let line = format!(
            "nxkit-pacing: diag present interval {mean:.2} ms (sd {sd:.2}, min {:.2}, max {:.2}) over {} presents",
            self.min, self.max, self.n
        );
        *self = Self {
            last: secs,
            ..Self::new()
        };
        Some(line)
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
    fn the_spread_shows_a_13_ms_frame_as_13_ms() {
        let mut p = PaceSpread::new();
        let mut t = 100.0;
        let mut line = None;
        for i in 0..=PACE_EVERY {
            // 13 ms with a swing of 1 ms either way.
            let swing = if i % 2 == 0 { 0.001 } else { -0.001 };
            t += 0.013 + swing;
            line = line.or(p.note(t));
        }
        assert!(line.is_some(), "a line after a full window");
        let line = line.unwrap_or_default();
        assert!(line.contains("interval 13.00 ms"), "{line}");
        assert!(line.contains("sd 1.00"), "{line}");
        assert!(line.contains("min 12.00, max 14.00"), "{line}");
        // The next window starts empty, and a dropped or repeated time makes no interval.
        assert_eq!(p.n, 0);
        assert_eq!(p.note(t), None);
        assert_eq!(p.n, 0);
    }

    #[test]
    fn a_present_is_known_until_the_ring_comes_round() {
        let s = Screen::default();
        s.shown(1001, 5.5);
        s.shown(1002, 0.0);
        assert_eq!(s.shown_at(1001), 5.5);
        assert_eq!(s.shown_at(1002), -1.0);
        assert_eq!(s.shown_at(1001 + RING as u64), 0.0);
        s.set_hold(1001, 3);
        assert_eq!(s.hold(1001), 3);
        assert_eq!(s.hold(1001 + RING as u64), 0);
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
