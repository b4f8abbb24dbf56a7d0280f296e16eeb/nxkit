//! A manual test of the rumble on a real controller: [`rumble_probe`] names the first controller, says how it rumbles
//! and plays a short pattern through the same path as a game. `examples/rumble_probe.rs` runs it:
//! `cargo run -p nxkit-svc-input --example rumble_probe` (add `-- --no-hd` to keep a Switch Pro Controller on Core
//! Haptics).

use std::time::{Duration, Instant};

use crate::config::Config;
use crate::rumble::VibrationValue;
use crate::with;

/// The vibration handles of player 1 on a full key controller: motor 0 (left) and motor 1 (right).
const HANDLES: [u32; 2] = [3, 3 | 1 << 16];
/// The time between two values of the pattern, the update rate of the Switch's vibration.
const STEP: Duration = Duration::from_millis(5);

fn value(amp_low: f32, freq_low: f32, amp_high: f32, freq_high: f32) -> VibrationValue {
    VibrationValue {
        amp_low,
        freq_low,
        amp_high,
        freq_high,
    }
}

/// The pattern at `ms` after its start: the name of the part and the value of each motor. `None` after the end.
fn pattern(ms: u64) -> Option<(&'static str, [VibrationValue; 2])> {
    let idle = VibrationValue::IDLE;
    match ms {
        // three low thumps on the left motor: 150 ms on, 250 ms off
        0..1200 => Some((
            "low thump, left motor, 80 Hz",
            if ms % 400 < 150 {
                [value(0.9, 80.0, 0.0, 320.0), idle]
            } else {
                [idle; 2]
            },
        )),
        1200..2200 => Some((
            "high buzz, right motor, 640 Hz",
            if ms < 2000 {
                [idle, value(0.0, 160.0, 0.6, 640.0)]
            } else {
                [idle; 2]
            },
        )),
        // six clicks of 30 ms, 200 ms apart, left and right in turn
        2200..3400 => {
            let k = (ms - 2200) / 200;
            let on = (ms - 2200) % 200 < 30;
            let click = value(1.0, 160.0, 1.0, 320.0);
            let sides = match (on, k % 2) {
                (false, _) => [idle; 2],
                (true, 0) => [click, idle],
                (true, _) => [idle, click],
            };
            Some(("clicks, left and right in turn", sides))
        }
        // both bands of both motors sweep up in two seconds: low 40 to 640 Hz, high 80 to 1250 Hz
        3400..5400 => {
            let x = (ms - 3400) as f32 / 2000.0;
            let v = value(
                0.5,
                40.0 * 16f32.powf(x),
                0.5,
                80.0 * (1250.0f32 / 80.0).powf(x),
            );
            Some(("sweep, both motors, 40 Hz to 1250 Hz", [v; 2]))
        }
        5400..5600 => Some(("rest", [idle; 2])),
        _ => None,
    }
}

/// Names the first controller and plays the pattern on it. `hd_rumble` is [`Config::hd_rumble`]. `say` gets the
/// lines to show. False when no controller came within five seconds.
pub fn rumble_probe(hd_rumble: bool, say: &mut dyn FnMut(&str)) -> bool {
    crate::init(&Config {
        hd_rumble,
        ..Config::default()
    });
    let Some(info) = with(|i| i.rumble_info(0, Duration::from_secs(5))) else {
        say("no controller: connect one and run the probe again");
        return false;
    };
    say(&info);
    let start = Instant::now();
    let mut part = "";
    while let Some((name, sides)) = pattern(start.elapsed().as_millis() as u64) {
        if name != part {
            say(name);
            part = name;
        }
        with(|i| {
            for (h, v) in HANDLES.iter().zip(sides) {
                i.send_vibration(*h, v);
            }
            i.tick();
        });
        std::thread::sleep(STEP);
    }
    with(|i| i.stop_rumble());
    say("done");
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pattern_has_five_parts_and_ends_at_rest() {
        let names: Vec<&str> = (0..6000)
            .step_by(5)
            .filter_map(pattern)
            .map(|p| p.0)
            .collect();
        let mut parts = names.clone();
        parts.dedup();
        assert_eq!(parts.len(), 5);
        assert_eq!(pattern(5599).map(|p| p.1), Some([VibrationValue::IDLE; 2]));
        assert_eq!(pattern(5600), None);
    }

    #[test]
    fn the_sweep_spans_both_bands() {
        let first = pattern(3400).unwrap().1[0];
        let last = pattern(5399).unwrap().1[1];
        assert_eq!((first.freq_low, first.freq_high), (40.0, 80.0));
        assert!(last.freq_low > 630.0 && last.freq_high > 1240.0);
    }

    #[test]
    fn the_clicks_change_sides() {
        let left = pattern(2210).unwrap().1;
        let right = pattern(2410).unwrap().1;
        assert!(left[0].amp_low > 0.0 && left[1].amp_low == 0.0);
        assert!(right[1].amp_low > 0.0 && right[0].amp_low == 0.0);
    }
}
