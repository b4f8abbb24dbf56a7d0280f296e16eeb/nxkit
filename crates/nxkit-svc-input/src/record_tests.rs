use super::*;
use crate::bind::{Action, action};
use crate::npad::RawKeys;
use crate::timeline::{Runner, Step, parse_step};
use std::time::Duration;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[test]
fn every_button_name_is_the_action_of_its_bit() {
    let mut all = 0;
    for (name, bit) in BUTTONS {
        assert_eq!(action(name), Ok(Some(Action::Button(bit))), "{name}");
        all |= bit;
    }
    assert_eq!(all, PHYSICAL);
}

#[test]
fn nothing_is_written_while_nothing_changes() {
    let t0 = Instant::now();
    let (mut r, head) = Recorder::new(Timing::Frames, t0, 0);
    assert_eq!(head.len(), 1);
    assert!(r.observe(0, [0; 4], t0, 0).is_empty());
    assert!(r.observe(0, [0; 4], t0 + ms(900), 50).is_empty());
    assert!(r.finish(t0, 50).is_empty());
}

#[test]
fn frame_timing_writes_the_frames_between_changes() {
    let t0 = Instant::now();
    let (mut r, head) = Recorder::new(Timing::Frames, t0, 0);
    assert!(head[0].starts_with("# nxkit input recording"));
    assert_eq!(
        r.observe(btn::A, [0; 4], t0, 30),
        ["wait frames 30", "hold a"]
    );
    // The next poll of the same frame adds no wait.
    assert_eq!(r.observe(btn::A | btn::B, [0; 4], t0, 30), ["hold b"]);
    assert_eq!(
        r.observe(btn::B, [0, 0, 0, 0], t0, 36),
        ["wait frames 6", "release a"]
    );
    assert_eq!(r.finish(t0, 40), ["wait frames 4", "release"]);
}

#[test]
fn a_recording_that_starts_late_waits_for_its_frame_first() {
    let t0 = Instant::now();
    let (mut r, head) = Recorder::new(Timing::Frames, t0, 900);
    assert_eq!(head[1], "wait frame 900");
    assert_eq!(
        r.observe(btn::X, [0; 4], t0, 910),
        ["wait frames 10", "hold x"]
    );
}

#[test]
fn ms_timing_writes_totals_without_drift() {
    let t0 = Instant::now();
    let (mut r, head) = Recorder::new(Timing::Ms, t0, 500);
    assert_eq!(head.len(), 1);
    assert_eq!(
        r.observe(btn::A, [0; 4], t0 + ms(1234), 7),
        ["wait 1234", "hold a"]
    );
    assert_eq!(
        r.observe(0, [0; 4], t0 + ms(1500), 9),
        ["wait 266", "release a"]
    );
}

#[test]
fn ms_is_the_fallback_while_no_frame_was_presented() {
    let t0 = Instant::now();
    let (mut r, _) = Recorder::new(Timing::Frames, t0, 0);
    assert_eq!(
        r.observe(btn::A, [0; 4], t0 + ms(400), 0),
        ["wait 400", "hold a"]
    );
}

#[test]
fn sticks_and_the_stick_direction_bits() {
    let t0 = Instant::now();
    let (mut r, _) = Recorder::new(Timing::Frames, t0, 0);
    // Bit 16 is the stick direction button that the service derives. It is not recorded.
    let l = r.observe(1 << 16, [32767, 0, 0, -16384], t0, 1);
    assert_eq!(
        l,
        [
            "wait frames 1",
            "stick l 1.00000 0.00000",
            "stick r 0.00000 -0.50002"
        ]
    );
    assert_eq!(
        r.observe(1 << 16, [0, 0, 0, -16384], t0, 2),
        ["wait frames 1", "stick l 0.00000 0.00000"]
    );
}

/// The buttons per frame that a replay of `lines` gives.
fn replay(lines: &[String], frames: u64, step_ms: u64) -> Vec<RawKeys> {
    let t0 = Instant::now();
    let steps: Vec<Step> = lines
        .iter()
        .filter(|l| !l.starts_with('#'))
        .map(|l| parse_step(l).unwrap_or_else(|e| panic!("{l}: {e}")))
        .collect();
    let mut run = Runner::new(steps, t0, 0);
    (0..frames)
        .map(|f| run.poll(t0 + ms(f * step_ms), f)[0])
        .collect()
}

#[test]
fn a_recording_replays_to_the_same_input_in_every_frame() {
    // A scripted game: what player 1 holds in each of 40 frames.
    let truth: Vec<(u64, [i32; 4])> = (0..40u64)
        .map(|f| {
            let b = match f {
                3..=6 => btn::A,
                7..=12 => btn::A | btn::ZR,
                20..=21 => btn::PLUS,
                _ => 0,
            };
            let s = if (10..30).contains(&f) {
                [20000, -3000, 0, 0]
            } else {
                [0; 4]
            };
            (b, s)
        })
        .collect();
    let t0 = Instant::now();
    let (mut rec, mut lines) = Recorder::new(Timing::Frames, t0, 0);
    for (f, (b, s)) in truth.iter().enumerate() {
        // The game polls twice in each frame.
        for _ in 0..2 {
            lines.extend(rec.observe(*b, *s, t0 + ms(f as u64 * 16), f as u64));
        }
    }
    lines.extend(rec.finish(t0 + ms(40 * 16), 40));
    // The replay runs with another speed of the wall clock and gives the same input in each frame.
    for step_ms in [16, 100] {
        let got = replay(&lines, 40, step_ms);
        for (f, (b, s)) in truth.iter().enumerate() {
            assert_eq!(got[f].buttons, *b, "frame {f}");
            for i in 0..4 {
                // A stick goes through five decimals, so it can differ by one step.
                assert!((got[f].sticks[i] - s[i]).abs() <= 1, "frame {f} axis {i}");
            }
        }
    }
}
