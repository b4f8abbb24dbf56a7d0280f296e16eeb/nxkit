use super::*;
use crate::mode::BadTarget;

const MS: u64 = 1_000_000;

fn sample(presents: u64, now_ns: u64) -> PresentSample {
    PresentSample {
        presents,
        now_ns,
        gpu_ms: None,
        snap: None,
        screen_hz: 0,
        link_active: false,
        movie: false,
        thermal: 0,
        low_power: false,
        perf: None,
    }
}

fn stepper(target: u32) -> LoadStepper {
    LoadStepper::new(config(target))
}

fn config(target: u32) -> LoadConfig {
    LoadConfig {
        target,
        hold: true,
        thermal_cap: true,
        probe_ramp: true,
        perf_levels: 1,
        work: false,
    }
}

/// Runs frames that each start `ms` after the last and returns the last outcome.
fn run(
    s: &mut LoadStepper,
    from: u64,
    n: u64,
    ms: f64,
    mut tweak: impl FnMut(&mut PresentSample),
) -> Outcome {
    // The outcome of the last frame, with the events of every frame of the run.
    let mut out: Option<Outcome> = None;
    let mut events = Vec::new();
    for i in 0..n {
        let now = ((from + i) as f64 * ms * 1e6) as u64 + MS;
        let mut p = sample(from + i, now);
        tweak(&mut p);
        s.frame_started(now, 0);
        out = s.present(&p, None).ok();
        if let Some(o) = &out {
            events.extend(o.events.iter().cloned());
        }
    }
    out.map(|o| Outcome { events, ..o })
        .unwrap_or_else(|| Outcome {
            cap: 0,
            hold: 0,
            step: 0,
            kmin: 0,
            events: Vec::new(),
        })
}

#[test]
fn a_config_takes_30_60_and_120_only() {
    assert!(LoadConfig::new(30).is_ok());
    assert!(LoadConfig::new(120).is_ok_and(|c| c.at_120()));
    assert_eq!(LoadConfig::new(45), Err(BadTarget(45)));
}

#[test]
fn steady_60_fps_frames_are_held_for_one_vsync() {
    let mut s = stepper(60);
    let out = run(&mut s, 1, 120, 1000.0 / 60.0, |_| {});
    assert_eq!((out.cap, out.hold, out.step, out.kmin), (60, 1, 1, 1));
    // Only the first present reports the cap; nothing changes after it.
    assert!(
        out.events
            .iter()
            .all(|e| matches!(e, Event::CapChanged { .. }))
    );
}

#[test]
fn the_first_present_reports_the_cap() {
    let mut s = stepper(60);
    let out = run(&mut s, 1, 1, 16.7, |_| {});
    assert_eq!(
        out.events,
        vec![Event::CapChanged {
            from: 0,
            to: 60,
            why: CapWhy::Target
        }]
    );
}

#[test]
fn a_movie_or_a_thermal_state_or_target_30_makes_the_cap_30() {
    let mut s = stepper(60);
    let out = run(&mut s, 1, 5, 33.3, |p| p.movie = true);
    assert_eq!((out.cap, out.kmin), (30, 2));
    let mut s = stepper(60);
    let out = run(&mut s, 1, 5, 33.3, |p| p.thermal = 2);
    assert_eq!(out.cap, 30);
    let mut s = stepper(30);
    let out = run(&mut s, 1, 5, 33.3, |_| {});
    assert_eq!((out.cap, out.hold), (30, 2));
}

#[test]
fn a_target_of_120_runs_in_ticks_and_falls_back_to_60_on_a_screen_below_120() {
    let mut s = stepper(120);
    let out = run(&mut s, 1, 120, 1000.0 / 120.0, |p| p.screen_hz = 120);
    assert_eq!((out.cap, out.hold, out.kmin), (120, 1, 1));
    let mut s = stepper(120);
    let out = run(&mut s, 1, 5, 1000.0 / 60.0, |p| p.screen_hz = 60);
    assert_eq!((out.cap, out.kmin), (60, 2));
    assert!(out.events.contains(&Event::CapChanged {
        from: 0,
        to: 60,
        why: CapWhy::ScreenBelow120
    }));
}

#[test]
fn late_frames_raise_the_hold_to_two_and_a_pay_back_follows() {
    let mut s = stepper(60);
    run(&mut s, 1, 60, 1000.0 / 60.0, |_| {});
    let out = run(&mut s, 61, 12, 25.0, |_| {});
    assert_eq!(out.hold, 2);
    assert!(s.stats().ups >= 1);
    assert!(s.stats().late > 0);
}

#[test]
fn a_hold_is_never_raised_when_fps_hold_is_off() {
    let mut cfg = config(60);
    cfg.hold = false;
    let mut s = LoadStepper::new(cfg);
    let out = run(&mut s, 1, 60, 25.0, |_| {});
    assert_eq!(out.hold, 1);
}

#[test]
fn the_screens_verdict_makes_a_frame_late() {
    let mut s = stepper(60);
    run(&mut s, 1, 20, 1000.0 / 60.0, |p| {
        p.link_active = true;
        p.snap = Some(SnapView {
            presented: -1.0,
            before: 0.0,
            expect_ms: 16.7,
        });
    });
    assert!(s.stats().late > 0);
}

#[test]
fn the_summary_comes_every_1800_presents() {
    let mut s = stepper(60);
    let out = run(&mut s, 1799, 2, 1000.0 / 60.0, |_| {});
    assert!(out.events.iter().any(|e| matches!(e, Event::Window { .. })));
}

#[test]
fn the_step_and_the_game_step_go_through_the_state_page() {
    let _g = crate::GUEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut mem = vec![0u64; 0x60 / 8 + 2];
    let base = mem.as_mut_ptr() as u64;
    nxkit_abi::guest::set_range(base, (mem.len() * 8) as u64);
    let Ok(page) = StatePage::new(base) else {
        return;
    };
    let mut s = stepper(60);
    for i in 1..=10u64 {
        let now = i * 16_667_000 + MS;
        s.frame_started(now, 0);
        let mut p = sample(i, now);
        p.movie = i > 5;
        assert!(s.present(&p, Some(&page)).is_ok());
    }
    assert_eq!(nxkit_abi::guest::read_u32(base + 0x20).ok(), Some(2));
    assert_eq!(nxkit_abi::guest::read_u32(base + 4).ok(), Some(1));
    assert!(nxkit_abi::guest::write_u32(base + 0x24, 2).is_ok());
    let mut p = sample(11, 11 * 16_667_000 + MS);
    p.movie = false;
    s.frame_started(p.now_ns, 0);
    let out = s.present(&p, Some(&page));
    assert!(out.is_ok_and(|o| o.events.contains(&Event::GameStep { vsyncs: 2 })));
}
