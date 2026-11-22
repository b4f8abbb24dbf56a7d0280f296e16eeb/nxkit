//! The rumble reports of a Switch Pro Controller, a port of `src/encoding/standard.rs` of RenHDrum
//! (<https://github.com/trzz1214/RenHDrum>, MIT OR Apache-2.0, used under MIT). The amplitude table, the frequency
//! formula and the layout of the reports are from dekuNukem's Nintendo_Switch_Reverse_Engineering
//! (`rumble_data_table.md`, `bluetooth_hid_notes.md`).
//!
//! A side of a report is 4 bytes and holds the high and the low band of one actuator. Report 0x10 carries only the
//! rumble of both sides. Report 0x01 carries the rumble and a subcommand.

use crate::rumble::VibrationValue;

/// The amplitude thresholds of the 101 steps of the table, on a scale of 0 to 65535.
const AMP_THRESHOLDS: [u16; 101] = [
    0, 514, 775, 921, 1096, 1303, 1550, 1843, 2192, 2606, 3100, 3686, 4383, 5213, 6199, 7372, 7698,
    8039, 8395, 8767, 9155, 9560, 9984, 10426, 10887, 11369, 11873, 12398, 12947, 13520, 14119,
    14744, 15067, 15397, 15734, 16079, 16431, 16790, 17158, 17534, 17918, 18310, 18711, 19121,
    19540, 19967, 20405, 20851, 21308, 21775, 22251, 22739, 23236, 23745, 24265, 24797, 25340,
    25894, 26462, 27041, 27633, 28238, 28856, 29488, 30134, 30794, 31468, 32157, 32861, 33581,
    34316, 35068, 35836, 36620, 37422, 38242, 39079, 39935, 40809, 41703, 42616, 43549, 44503,
    45477, 46473, 47491, 48531, 49593, 50679, 51789, 52923, 54082, 55266, 56476, 57713, 58977,
    60268, 61588, 62936, 64315, 65535,
];

/// The side of an actuator at rest: no amplitude at 160 Hz and 320 Hz.
pub(crate) const NEUTRAL: [u8; 4] = [0x00, 0x01, 0x40, 0x40];

/// The report id of a rumble report.
pub(crate) const RUMBLE_REPORT: u8 = 0x10;
/// The report id of a rumble report with a subcommand.
const SUBCOMMAND_REPORT: u8 = 0x01;
/// The subcommand that turns the actuators on (argument 1).
const ENABLE_VIBRATION: u8 = 0x48;

/// The step of the table for an amplitude of 0 to 1.
fn amp_index(a: f32) -> usize {
    if a.is_nan() || a <= 0.0 {
        return 0;
    }
    let raw = (a.min(1.0) * 65535.0) as u16;
    AMP_THRESHOLDS
        .partition_point(|&t| t < raw)
        .min(AMP_THRESHOLDS.len() - 1)
}

/// The code of a frequency: `round(log2(f / 10) * 32)`, ties to even as in the original.
fn freq_code(f: f32) -> i32 {
    ((f64::from(f) / 10.0).log2() * 32.0).round_ties_even() as i32
}

/// The 4 bytes of one side. Both bands together above full scale are scaled down to it.
pub(crate) fn encode_side(v: &VibrationValue) -> [u8; 4] {
    let (mut lo, mut hi) = (v.amp_low, v.amp_high);
    if lo > 0.0 && hi > 0.0 && lo + hi > 1.0 {
        let k = 1.0 / (lo + hi);
        lo *= k;
        hi *= k;
    }
    let (hi_idx, lo_idx) = (amp_index(hi), amp_index(lo));
    if hi_idx == 0 && lo_idx == 0 {
        return NEUTRAL;
    }
    let freq = |f: f32, default: f32| if f > 0.0 { f } else { default };
    let hf = ((freq_code(freq(v.freq_high, 320.0)) - 0x60) * 4).clamp(0x00, 0x1FC);
    let lf = (freq_code(freq(v.freq_low, 160.0)) - 0x40).clamp(0x01, 0x7F);
    let hf_amp = (hi_idx * 2) as i32;
    let lf_amp = if lo_idx & 1 != 0 { 0x8000 } else { 0 } | (0x40 + lo_idx / 2) as i32;
    [
        (hf & 0xFF) as u8,
        (hf_amp | ((hf >> 8) & 0x01)) as u8,
        (lf | ((lf_amp >> 8) & 0x80)) as u8,
        (lf_amp & 0xFF) as u8,
    ]
}

/// True when a side has no amplitude in either band.
pub(crate) fn is_neutral(side: &[u8; 4]) -> bool {
    side[1] & 0xFE == 0 && side[3] == 0x40 && side[2] & 0x80 == 0
}

/// Report 0x10: the counter (0 to 15) and the left and the right side.
pub(crate) fn rumble_report(counter: u8, sides: &[[u8; 4]; 2]) -> [u8; 10] {
    let mut r = [0u8; 10];
    r[0] = RUMBLE_REPORT;
    r[1] = counter & 0x0F;
    r[2..6].copy_from_slice(&sides[0]);
    r[6..10].copy_from_slice(&sides[1]);
    r
}

/// Report 0x01 with subcommand 0x48 and argument 1: the actuators are switched on. Both sides are at rest.
pub(crate) fn enable_report(counter: u8) -> [u8; 12] {
    let mut r = [0u8; 12];
    r[0] = SUBCOMMAND_REPORT;
    r[1] = counter & 0x0F;
    r[2..6].copy_from_slice(&NEUTRAL);
    r[6..10].copy_from_slice(&NEUTRAL);
    r[10] = ENABLE_VIBRATION;
    r[11] = 1;
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(amp_low: f32, freq_low: f32, amp_high: f32, freq_high: f32) -> [u8; 4] {
        encode_side(&VibrationValue {
            amp_low,
            freq_low,
            amp_high,
            freq_high,
        })
    }

    fn high_freq(s: &[u8; 4]) -> i32 {
        s[0] as i32 | ((s[1] & 0x01) as i32) << 8
    }

    fn high_amp(s: &[u8; 4]) -> i32 {
        (s[1] & 0xFE) as i32
    }

    fn low_freq(s: &[u8; 4]) -> i32 {
        (s[2] & 0x7F) as i32
    }

    fn low_amp(s: &[u8; 4]) -> i32 {
        ((s[2] & 0x80) as i32) << 8 | s[3] as i32
    }

    #[test]
    fn the_table_is_the_published_one() {
        assert_eq!(
            AMP_THRESHOLDS.iter().map(|&t| u64::from(t)).sum::<u64>(),
            2_595_520
        );
        assert!(AMP_THRESHOLDS.windows(2).all(|w| w[0] < w[1]));
    }

    // The golden values of RenHDrum's tests of `standard.rs`.
    #[test]
    fn zero_amplitude_is_neutral() {
        assert_eq!(encode(0.0, 160.0, 0.0, 320.0), NEUTRAL);
        assert_eq!(encode(0.0, 0.0, 0.0, 0.0), NEUTRAL);
        assert_eq!(encode(-1.0, 160.0, f32::NAN, 320.0), NEUTRAL);
        assert!(is_neutral(&NEUTRAL) && !is_neutral(&encode(0.5, 160.0, 0.0, 320.0)));
    }

    #[test]
    fn the_amplitude_follows_the_table() {
        for (a, high, low) in [
            (0.0078f32, 0x02, 0x8040),
            (0.0118, 0x04, 0x0041),
            (0.1226, 0x22, 0x8048),
            (0.2562, 0x4A, 0x8052),
            (0.5014, 0x88, 0x0062),
            (0.9813, 0xC6, 0x8071),
            (1.0, 0xC8, 0x0072),
        ] {
            let h = encode(0.0, 160.0, a, 320.0);
            assert_eq!((high_amp(&h), low_amp(&h)), (high, 0x0040), "high {a}");
            let l = encode(a, 160.0, 0.0, 320.0);
            assert_eq!((low_amp(&l), high_amp(&l)), (low, 0), "low {a}");
        }
        assert_eq!(high_amp(&encode(0.0, 160.0, 1.5, 320.0)), 0xC8);
    }

    #[test]
    fn the_frequencies_follow_the_formula() {
        for (f, code) in [(40.0f32, 0x01), (80.0, 0x20), (160.0, 0x40), (320.0, 0x60)] {
            assert_eq!(low_freq(&encode(0.5014, f, 0.0, 320.0)), code, "low {f}");
        }
        for (f, code) in [
            (160.0f32, 0x80),
            (320.0, 0x100),
            (1252.0, 0x1FC),
            (3000.0, 0x1FC),
        ] {
            assert_eq!(high_freq(&encode(0.0, 160.0, 0.5014, f)), code, "high {f}");
        }
    }

    #[test]
    fn a_side_combines_both_bands() {
        assert_eq!(
            encode(0.2562, 160.0, 0.5014, 320.0),
            [0x00, 0x89, 0xC0, 0x52]
        );
        assert_eq!(encode(1.0, 40.0, 0.0, 0.0), [0x00, 0x01, 0x01, 0x72]);
        assert_eq!(encode(0.0, 0.0, 1.0, 1252.0), [0xFC, 0xC9, 0x40, 0x40]);
        assert_eq!(
            encode(0.8, 160.0, 0.8, 320.0),
            encode(0.5, 160.0, 0.5, 320.0)
        );
        assert_eq!(
            encode(1.5, 160.0, 0.5, 320.0),
            encode(0.75, 160.0, 0.25, 320.0)
        );
    }

    #[test]
    fn the_reports_carry_the_counter_and_both_sides() {
        let s = [encode(0.5, 160.0, 0.0, 320.0), NEUTRAL];
        let r = rumble_report(17, &s);
        assert_eq!(&r[..2], &[0x10, 1]);
        assert_eq!((&r[2..6], &r[6..10]), (&s[0][..], &NEUTRAL[..]));
        assert_eq!(
            enable_report(3),
            [
                0x01, 3, 0x00, 0x01, 0x40, 0x40, 0x00, 0x01, 0x40, 0x40, 0x48, 0x01
            ]
        );
    }
}
