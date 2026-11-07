//! Switch vibration values and how they become one haptic level, a port of nxnative `hle_haptics.c` (MIT).
//!
//! A Switch rumble motor has a low and a high band, each with an amplitude and a frequency. A Mac controller has an
//! intensity and a sharpness per engine. The mapping uses the power of both bands for the intensity and the
//! frequency of the power for the sharpness.

use crate::npad::PLAYERS;

/// The lowest frequency a motor plays, in Hz.
const FREQ_MIN: f32 = 40.0;
/// The highest frequency a motor plays, in Hz.
const FREQ_MAX: f32 = 1250.0;
const FREQ_LOW_DEFAULT: f32 = 160.0;
const FREQ_HIGH_DEFAULT: f32 = 320.0;

/// `nn::hid::VibrationValue`, 16 bytes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct VibrationValue {
    pub(crate) amp_low: f32,
    pub(crate) freq_low: f32,
    pub(crate) amp_high: f32,
    pub(crate) freq_high: f32,
}

impl VibrationValue {
    /// The value of a motor that does not vibrate.
    pub(crate) const IDLE: VibrationValue = VibrationValue {
        amp_low: 0.0,
        freq_low: FREQ_LOW_DEFAULT,
        amp_high: 0.0,
        freq_high: FREQ_HIGH_DEFAULT,
    };

    /// The value from the 16 bytes the game wrote.
    pub(crate) fn from_bytes(b: &[u8; 16]) -> VibrationValue {
        let f = |i: usize| f32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        VibrationValue { amp_low: f(0), freq_low: f(4), amp_high: f(8), freq_high: f(12) }
    }

    /// The 16 bytes of the value.
    pub(crate) fn to_bytes(self) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[0..4].copy_from_slice(&self.amp_low.to_le_bytes());
        b[4..8].copy_from_slice(&self.freq_low.to_le_bytes());
        b[8..12].copy_from_slice(&self.amp_high.to_le_bytes());
        b[12..16].copy_from_slice(&self.freq_high.to_le_bytes());
        b
    }

    /// Amplitudes in 0 to 1 (a value that is not finite is 0) and frequencies in 40 to 1250 Hz. A frequency that is
    /// not finite or not positive is the default of its band.
    pub(crate) fn sanitized(self) -> VibrationValue {
        let amp = |a: f32| if a.is_finite() { a.clamp(0.0, 1.0) } else { 0.0 };
        let freq = |f: f32, default: f32| {
            if f.is_finite() && f > 0.0 { f.clamp(FREQ_MIN, FREQ_MAX) } else { default }
        };
        VibrationValue {
            amp_low: amp(self.amp_low),
            freq_low: freq(self.freq_low, FREQ_LOW_DEFAULT),
            amp_high: amp(self.amp_high),
            freq_high: freq(self.freq_high, FREQ_HIGH_DEFAULT),
        }
    }
}

/// What one Core Haptics engine plays.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Level {
    /// 0 to 1. Zero is silence.
    pub(crate) intensity: f32,
    /// 0 to 1.
    pub(crate) sharpness: f32,
}

/// The level of some motors that play together. `gain` scales the intensity.
pub(crate) fn level(values: &[VibrationValue], gain: f32) -> Level {
    let mut power = 0.0f32;
    let mut weighted = 0.0f32;
    for v in values {
        let v = v.sanitized();
        let lo = v.amp_low * v.amp_low;
        let hi = v.amp_high * v.amp_high;
        power += lo + hi;
        weighted += lo * (v.freq_low / FREQ_MIN).ln() + hi * (v.freq_high / FREQ_MIN).ln();
    }
    let gain = if gain.is_finite() && gain >= 0.0 { gain } else { 1.0 };
    let intensity = (power.sqrt() * gain).min(1.0);
    let sharpness = if power > 0.0 {
        (weighted / (power * (FREQ_MAX / FREQ_MIN).ln())).clamp(0.0, 1.0)
    } else {
        0.0
    };
    Level { intensity, sharpness }
}

/// The intensity that the controller gets: nothing at or below `min`, then 0 to 1 above it.
pub(crate) fn controller_level(x: f32, min: f32) -> f32 {
    let min = if min.is_finite() { min.clamp(0.0, 0.99) } else { 0.0 };
    if x <= min { 0.0 } else { ((x - min) / (1.0 - min)).min(1.0) }
}

/// The levels of one player: each motor alone, for a controller with a left and a right engine, and both motors
/// together, for a controller with one engine.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct PlayerLevels {
    /// Motor 0 and motor 1.
    pub(crate) motors: [Level; 2],
    /// Both motors.
    pub(crate) mixed: Level,
}

/// The last value the game sent to each motor of each player.
#[derive(Clone, Debug)]
pub(crate) struct Rumble {
    values: [[VibrationValue; 2]; PLAYERS],
}

impl Default for Rumble {
    fn default() -> Self {
        Rumble { values: [[VibrationValue::IDLE; 2]; PLAYERS] }
    }
}

impl Rumble {
    /// Stores the value of a motor. Returns false when the sanitised value is the one already stored.
    pub(crate) fn set(&mut self, player: usize, motor: usize, v: VibrationValue) -> bool {
        let v = v.sanitized();
        match self.values.get_mut(player).and_then(|p| p.get_mut(motor)) {
            Some(slot) if *slot != v => {
                *slot = v;
                true
            }
            _ => false,
        }
    }

    /// The value of a motor.
    pub(crate) fn get(&self, player: usize, motor: usize) -> VibrationValue {
        self.values
            .get(player)
            .and_then(|p| p.get(motor))
            .copied()
            .unwrap_or(VibrationValue::IDLE)
    }

    /// Every motor back to idle.
    pub(crate) fn clear(&mut self) {
        *self = Rumble::default();
    }

    /// The levels of a player for a given gain and controller minimum.
    pub(crate) fn levels(&self, player: usize, gain: f32, min: f32) -> PlayerLevels {
        let Some(p) = self.values.get(player) else {
            return PlayerLevels::default();
        };
        let map = |l: Level| Level { intensity: controller_level(l.intensity, min), sharpness: l.sharpness };
        PlayerLevels {
            motors: [map(level(&p[0..1], gain)), map(level(&p[1..2], gain))],
            mixed: map(level(&p[..], gain)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(al: f32, fl: f32, ah: f32, fh: f32) -> VibrationValue {
        VibrationValue { amp_low: al, freq_low: fl, amp_high: ah, freq_high: fh }
    }

    #[test]
    fn idle_is_silent() {
        let l = level(&[VibrationValue::IDLE], 1.0);
        assert_eq!(l, Level { intensity: 0.0, sharpness: 0.0 });
    }

    #[test]
    fn sanitising_clamps_and_repairs() {
        let s = v(2.0, 5.0, f32::NAN, f32::NAN).sanitized();
        assert_eq!(s, v(1.0, 40.0, 0.0, 320.0));
        let s = v(0.5, 0.0, 0.5, 9999.0).sanitized();
        assert_eq!(s, v(0.5, 160.0, 0.5, 1250.0));
    }

    #[test]
    fn the_intensity_is_the_root_of_the_power() {
        let l = level(&[v(0.6, 160.0, 0.0, 320.0)], 1.0);
        assert!((l.intensity - 0.6).abs() < 1e-5);
        let l = level(&[v(1.0, 160.0, 1.0, 320.0)], 1.0);
        assert_eq!(l.intensity, 1.0);
        let l = level(&[v(0.2, 160.0, 0.0, 320.0)], 2.0);
        assert!((l.intensity - 0.4).abs() < 1e-5);
    }

    #[test]
    fn a_high_frequency_is_sharper() {
        let low = level(&[v(0.5, 40.0, 0.0, 320.0)], 1.0);
        let high = level(&[v(0.5, 1250.0, 0.0, 320.0)], 1.0);
        assert_eq!(low.sharpness, 0.0);
        assert!((high.sharpness - 1.0).abs() < 1e-5);
    }

    #[test]
    fn the_controller_minimum_removes_weak_rumble() {
        assert_eq!(controller_level(0.1, 0.2), 0.0);
        assert!((controller_level(0.6, 0.2) - 0.5).abs() < 1e-6);
        assert_eq!(controller_level(1.0, 0.0), 1.0);
    }

    #[test]
    fn an_unchanged_value_is_not_stored_again() {
        let mut r = Rumble::default();
        assert!(r.set(0, 0, v(0.5, 160.0, 0.0, 320.0)));
        assert!(!r.set(0, 0, v(0.5, 160.0, 0.0, 320.0)));
        assert!(!r.set(9, 0, v(0.5, 160.0, 0.0, 320.0)));
        assert_eq!(r.get(0, 0).amp_low, 0.5);
        r.clear();
        assert_eq!(r.get(0, 0), VibrationValue::IDLE);
    }

    #[test]
    fn a_motor_plays_alone_or_mixed() {
        let mut r = Rumble::default();
        r.set(0, 1, v(0.0, 160.0, 0.8, 320.0));
        let l = r.levels(0, 1.0, 0.0);
        assert_eq!(l.motors[0].intensity, 0.0);
        assert!((l.motors[1].intensity - 0.8).abs() < 1e-5);
        assert!((l.mixed.intensity - 0.8).abs() < 1e-5);
    }

    #[test]
    fn the_bytes_round_trip() {
        let x = v(0.25, 100.0, 0.5, 400.0);
        assert_eq!(VibrationValue::from_bytes(&x.to_bytes()), x);
    }
}
