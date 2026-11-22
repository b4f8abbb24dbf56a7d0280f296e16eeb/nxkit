use super::*;
use crate::npad::btn;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn set(buttons: u64, ms: Option<u64>) -> Step {
    Step::Set {
        player: 0,
        buttons,
        sticks: [None; 4],
        ms,
    }
}

fn steps(text: &str) -> Vec<Step> {
    text.lines()
        .map(|l| parse_step(l).unwrap_or_else(|e| panic!("{l}: {e}")))
        .collect()
}

#[test]
fn press_hold_and_release_parse() {
    assert_eq!(parse_step("press a"), Ok(set(btn::A, Some(100))));
    assert_eq!(parse_step("PRESS A 250"), Ok(set(btn::A, Some(250))));
    assert_eq!(
        parse_step("press a+zr dpad_up 30"),
        Ok(set(btn::A | btn::ZR | btn::UP, Some(30)))
    );
    assert_eq!(parse_step("hold plus"), Ok(set(btn::PLUS, None)));
    assert_eq!(
        parse_step("hold plus+minus"),
        Ok(set(btn::PLUS | btn::MINUS, None))
    );
    assert_eq!(
        parse_step("release a b"),
        Ok(Step::Release {
            player: 0,
            buttons: btn::A | btn::B,
            sticks: [false; 4]
        })
    );
    assert_eq!(
        parse_step("release"),
        Ok(Step::Release {
            player: 0,
            buttons: u64::MAX,
            sticks: [true; 4]
        })
    );
}

#[test]
fn the_player_is_the_trailing_pair() {
    assert_eq!(
        parse_step("press a 10 player 2"),
        Ok(Step::Set {
            player: 1,
            buttons: btn::A,
            sticks: [None; 4],
            ms: Some(10)
        })
    );
    assert!(parse_step("press a player 0").is_err());
    assert!(parse_step("press a player 9").is_err());
    assert!(parse_step("press a player x").is_err());
}

#[test]
fn sticks_parse_and_clamp() {
    assert_eq!(
        parse_step("stick l 1 -0.5"),
        Ok(Step::Set {
            player: 0,
            buttons: 0,
            sticks: [Some(32767), Some(-16383), None, None],
            ms: None
        })
    );
    let Ok(Step::Set { sticks, ms, .. }) = parse_step("stick r 3 0 400") else {
        panic!("not parsed");
    };
    assert_eq!(sticks, [None, None, Some(32767), Some(0)]);
    assert_eq!(ms, Some(400));
    assert!(parse_step("stick x 0 0").is_err());
    assert!(parse_step("stick l 0").is_err());
    assert!(parse_step("stick l nan 0").is_err());
    let Ok(Step::Set { sticks, .. }) = parse_step("hold ls_up") else {
        panic!("not parsed");
    };
    assert_eq!(sticks, [None, Some(32767), None, None]);
}

#[test]
fn waits_parse() {
    assert_eq!(parse_step("wait 500"), Ok(Step::Wait(Wait::Ms(500))));
    assert_eq!(parse_step("wait 500ms"), Ok(Step::Wait(Wait::Ms(500))));
    assert_eq!(parse_step("wait 2.5s"), Ok(Step::Wait(Wait::Ms(2500))));
    assert_eq!(
        parse_step("wait frames 12"),
        Ok(Step::Wait(Wait::Frames(12)))
    );
    assert_eq!(
        parse_step("wait frame 900"),
        Ok(Step::Wait(Wait::UntilFrame(900)))
    );
    assert!(parse_step("wait").is_err());
    assert!(parse_step("wait -3").is_err());
    assert!(parse_step("wait 99999999999").is_err());
    assert!(parse_step("wait frames x").is_err());
}

#[test]
fn bad_commands_say_why() {
    for bad in [
        "",
        "jump a",
        "press",
        "press jump",
        "press a b+jump",
        "hold walk",
        "press 100",
    ] {
        assert!(parse_step(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn merging_ors_buttons_and_takes_the_larger_stick() {
    let a = RawKeys {
        buttons: btn::A,
        sticks: [100, -5000, 0, 0],
    };
    let b = RawKeys {
        buttons: btn::B,
        sticks: [-200, 4000, 0, 7],
    };
    assert_eq!(
        merge_keys(a, b),
        RawKeys {
            buttons: btn::A | btn::B,
            sticks: [-200, -5000, 0, 7]
        }
    );
}

#[test]
fn a_press_lasts_its_time_and_a_hold_lasts_until_the_release() {
    let t0 = Instant::now();
    let mut r = Runner::new(steps("press a 100\nhold b"), t0, 0);
    assert_eq!(r.poll(t0, 0)[0].buttons, btn::A | btn::B);
    assert_eq!(r.poll(t0 + ms(99), 0)[0].buttons, btn::A | btn::B);
    assert_eq!(r.poll(t0 + ms(100), 0)[0].buttons, btn::B);
    r.apply(
        &parse_step("release b").unwrap_or(set(0, None)),
        t0 + ms(200),
    );
    assert_eq!(r.poll(t0 + ms(200), 0)[0].buttons, 0);
}

#[test]
fn a_press_shorter_than_the_gap_between_polls_is_still_shown_once() {
    let t0 = Instant::now();
    let mut r = Runner::new(steps("press a 10"), t0, 0);
    assert_eq!(r.poll(t0 + ms(500), 0)[0].buttons, btn::A);
    assert_eq!(r.poll(t0 + ms(516), 0)[0].buttons, 0);
}

#[test]
fn a_wait_on_the_wall_clock_does_not_drift() {
    let t0 = Instant::now();
    let mut r = Runner::new(steps("wait 100\npress a 50\nwait 100\npress b 50"), t0, 0);
    assert_eq!(r.poll(t0 + ms(99), 0)[0].buttons, 0);
    // A late poll at 130 ms runs the press at its due time of 100 ms.
    assert_eq!(r.poll(t0 + ms(130), 0)[0].buttons, btn::A);
    // The second wait ends at 200 ms, not at 230 ms.
    assert_eq!(r.poll(t0 + ms(160), 0)[0].buttons, 0);
    assert_eq!(r.poll(t0 + ms(200), 0)[0].buttons, btn::B);
    assert!(r.done());
}

#[test]
fn waits_for_frames_count_frames_and_catch_up() {
    let t0 = Instant::now();
    let mut r = Runner::new(
        steps("wait frames 3\nhold a\nwait frames 2\nrelease a\nwait frames 1\nhold b"),
        t0,
        10,
    );
    assert_eq!(r.poll(t0, 12)[0].buttons, 0);
    assert_eq!(r.poll(t0, 13)[0].buttons, btn::A);
    assert_eq!(r.poll(t0, 14)[0].buttons, btn::A);
    assert_eq!(r.poll(t0, 15)[0].buttons, 0);
    // The poll at frame 20 is late. Every wait is due, so the last hold runs.
    assert_eq!(r.poll(t0, 20)[0].buttons, btn::B);
    assert!(r.done());
}

#[test]
fn the_same_frames_give_the_same_input_whatever_the_wall_clock() {
    let run = |step_ms: u64| {
        let t0 = Instant::now();
        let mut r = Runner::new(
            steps("wait frames 2\nhold a\nwait frames 3\nrelease a"),
            t0,
            0,
        );
        (0..8u64)
            .map(|f| r.poll(t0 + ms(f * step_ms), f)[0].buttons)
            .collect::<Vec<_>>()
    };
    assert_eq!(run(16), run(400));
    assert_eq!(run(16), [0, 0, btn::A, btn::A, btn::A, 0, 0, 0]);
}

#[test]
fn a_wait_until_a_frame_waits_for_that_frame() {
    let t0 = Instant::now();
    let mut r = Runner::new(
        steps("wait frame 100\nhold a\nwait frames 1\nrelease a"),
        t0,
        0,
    );
    assert_eq!(r.poll(t0, 99)[0].buttons, 0);
    assert_eq!(r.poll(t0, 100)[0].buttons, btn::A);
    assert_eq!(r.poll(t0, 101)[0].buttons, 0);
}

#[test]
fn sticks_hold_and_release_per_player() {
    let t0 = Instant::now();
    let mut r = Runner::new(
        steps("stick l 1 0\nhold a player 2\nstick r 0 1 100"),
        t0,
        0,
    );
    let out = r.poll(t0, 0);
    assert_eq!(out[0].sticks, [32767, 0, 0, 32767]);
    assert_eq!(out[1].buttons, btn::A);
    let out = r.poll(t0 + ms(100), 0);
    assert_eq!(out[0].sticks, [32767, 0, 0, 0]);
    r.apply(&parse_step("release").unwrap_or(set(0, None)), t0);
    assert_eq!(r.poll(t0 + ms(101), 0)[0], RawKeys::default());
}

#[test]
fn a_release_ends_a_timed_press_too() {
    let t0 = Instant::now();
    let mut r = Runner::new(steps("press a 1000\nrelease a"), t0, 0);
    assert_eq!(r.poll(t0, 0)[0].buttons, 0);
}

#[test]
fn progress_counts_the_steps() {
    let t0 = Instant::now();
    let mut r = Runner::new(steps("hold a\nwait 1000\nrelease"), t0, 0);
    r.poll(t0, 0);
    assert_eq!(r.progress(), (1, 3));
    assert!(!r.done());
}
