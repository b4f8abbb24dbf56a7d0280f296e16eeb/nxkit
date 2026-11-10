//! Cases of `dm/gfx/tests/t_fstep.c`, read on 2026-11-07. Frames are simulated in vsyncs. A frame whose work
//! fits its hold stays on screen for the hold. A longer one takes its work, because the next frame starts at once.

use super::*;

#[derive(Default)]
struct Sim {
    s: FrameStep,
    real: f64,
    game: f64,
    next: i32,
    slow: bool,
}

impl Sim {
    fn run(&mut self, n: usize, w: f64, kmin: i32, kmax: i32, fixed: i32) {
        for _ in 0..n {
            let k = if self.s.k != 0 { self.s.k } else { kmin };
            let step = if self.next != 0 { self.next } else { kmin };
            let shown = w.max(f64::from(k));
            self.real += shown;
            self.game += f64::from(step);
            self.next = self.s.next(&FrameInput {
                shown,
                cost: w,
                kmin,
                kmax,
                fixed,
                slow: self.slow,
            });
        }
    }

    fn call(&mut self, shown: f64, cost: f64, kmin: i32, kmax: i32) {
        self.next = self.s.next(&FrameInput {
            shown,
            cost,
            kmin,
            kmax,
            fixed: 0,
            slow: false,
        });
    }

    fn paid_back(&self) -> bool {
        let d = self.real - self.game - self.s.debt;
        d > -1.01 && d < 1.01
    }
}

#[test]
fn a_steady_60_holds_one_and_steps_one() {
    let mut m = Sim::default();
    m.run(600, 0.5, 1, 3, 0);
    assert!(m.s.k == 1 && m.s.ups == 0 && m.s.paid == 0);
    assert_eq!(m.game, m.real);
}

#[test]
fn steady_22_ms_frames_step_to_30_fps_without_flapping() {
    let mut m = Sim::default();
    m.run(120, 0.5, 1, 3, 0);
    m.run(600, 1.32, 1, 3, 0);
    assert!(m.s.k == 2 && m.s.ups == 1 && m.s.downs == 0);
    assert!(m.paid_back() && m.s.debt < 1.0);
}

#[test]
fn a_hitch_is_paid_back_with_one_extra_vsync_per_frame() {
    let mut m = Sim::default();
    m.run(60, 0.5, 1, 3, 0);
    m.real += 6.0;
    m.game += f64::from(m.next);
    m.call(6.0, 6.0, 1, 3);
    assert!(m.next == 2 && m.s.k == 1);
    let mut two = 0;
    for _ in 0..20 {
        let st = m.next;
        m.run(1, 0.5, 1, 3, 0);
        two += i32::from(st == 2);
    }
    assert_eq!(two, 5);
    assert!(m.game == m.real && m.s.k == 1);
}

#[test]
fn a_long_stall_is_dropped_and_not_replayed() {
    let mut m = Sim::default();
    m.run(60, 0.5, 1, 3, 0);
    m.real += 30.0;
    m.game += f64::from(m.next);
    m.call(30.0, 30.0, 1, 3);
    m.run(60, 0.5, 1, 3, 0);
    assert!(m.s.dropped == 14.0 && m.s.paid == 15 && m.real - m.game == 14.0 && m.s.k == 1);
}

#[test]
fn a_cheap_scene_returns_to_hold_one_after_a_trial() {
    let mut m = Sim::default();
    m.run(60, 0.5, 1, 3, 0);
    m.run(300, 1.3, 1, 3, 0);
    assert_eq!(m.s.k, 2);
    m.run(29, 0.6, 1, 3, 0);
    assert_eq!(m.s.k, 2);
    m.run(1, 0.6, 1, 3, 0);
    assert!(m.s.k == 1 && m.s.trial == FSTEP_RUN);
    m.run(FSTEP_RUN as usize + 10, 0.6, 1, 3, 0);
    assert!(m.s.k == 1 && m.s.trial == 0 && m.s.hold == FSTEP_HOLD0 && m.s.failed == 0);
}

#[test]
fn the_30_fps_cap_holds_two_and_pays_back_with_three() {
    let mut m = Sim::default();
    m.run(300, 0.5, 2, 2, 0);
    assert!(m.s.k == 2 && m.next == 2 && m.game == m.real);
    m.real += 6.0;
    m.game += f64::from(m.next);
    m.call(6.0, 6.0, 2, 2);
    let mut three = 0;
    for _ in 0..10 {
        let st = m.next;
        m.run(1, 0.5, 2, 2, 0);
        three += i32::from(st == 3);
    }
    assert!(three == 4 && m.s.k == 2 && m.game == m.real);
    m.run(300, 3.5, 2, 2, 0);
    assert_eq!(m.s.k, 2);
}

#[test]
fn a_movie_flag_changes_the_hold_at_once() {
    let mut m = Sim::default();
    m.run(60, 0.5, 1, 3, 0);
    m.run(1, 0.5, 2, 3, 0);
    assert_eq!(m.s.k, 2);
    m.run(120, 0.5, 2, 3, 0);
    m.run(1, 0.5, 1, 3, 0);
    assert!(m.s.k == 1 && m.s.ups == 0 && m.s.late == 0);
}

#[test]
fn a_cap_release_is_a_probe_with_all_its_grace() {
    let mut m = Sim::default();
    m.run(600, 0.86, 1, 3, 0);
    m.run(600, 1.5, 2, 2, 0);
    m.run(1, 1.5, 1, 3, 0);
    assert!(m.s.k == 1 && m.s.grace == FSTEP_GRACE && m.s.trial == FSTEP_RUN && m.s.probes == 1);
    let mut frames = 0;
    while m.s.k == 1 && frames < 100 {
        m.run(1, 1.3, 1, 3, 0);
        frames += 1;
    }
    assert!(
        frames == FSTEP_GRACE + 3 && m.s.k == 2 && m.s.failed == 1 && m.s.hold == 2.0 * FSTEP_HOLD0
    );
}

#[test]
fn the_games_own_step_is_held_and_ends_without_a_late_frame() {
    let mut m = Sim::default();
    m.run(60, 0.5, 1, 3, 0);
    m.run(120, 0.5, 1, 3, 2);
    assert!(m.s.k == 2 && m.next == 2 && m.s.debt == 0.0);
    m.run(2, 0.5, 1, 3, 0);
    assert!(m.s.k == 1 && m.next == 1 && m.s.late == 0);
}

#[test]
fn a_slow_cpu_without_the_report_starts_no_probe() {
    let mut m = Sim::default();
    m.run(60, 0.5, 1, 3, 0);
    m.run(300, 1.3, 1, 3, 0);
    m.run(2000, 1.2, 1, 3, 0);
    assert!(m.s.k == 2 && m.s.downs == 0 && m.s.probes == 0);
}

#[test]
fn a_slow_cpu_report_starts_one_probe_that_holds() {
    let mut m = Sim::default();
    m.run(60, 0.5, 1, 3, 0);
    m.run(300, 1.3, 1, 3, 0);
    m.slow = true;
    let ups = m.s.ups;
    let (mut at1, mut up_in_grace) = (0, false);
    for _ in 0..400 {
        let w = if m.s.k == 1 {
            at1 += 1;
            if at1 <= 20 { 1.5 } else { 0.6 }
        } else {
            1.2
        };
        m.run(1, w, 1, 3, 0);
        up_in_grace |= m.s.grace > 0 && m.s.ups != ups;
    }
    assert!(m.s.k == 1 && m.s.probes == 1 && m.s.failed == 0 && m.s.ups == ups && !up_in_grace);
    assert!(m.s.trial == 0 && m.s.probe == 2 && m.s.hold == FSTEP_HOLD0);
    assert!(m.paid_back() && m.s.dropped == 0.0);
}

#[test]
fn a_slow_report_at_hold_one_probes_nothing() {
    let mut m = Sim {
        slow: true,
        ..Sim::default()
    };
    m.run(600, 0.5, 1, 3, 0);
    assert!(m.s.k == 1 && m.s.probes == 0 && m.s.downs == 0 && m.game == m.real);
}

#[test]
fn the_screen_verdict_holds_for_one_call() {
    let mut m = Sim::default();
    m.run(120, 0.5, 1, 3, 0);
    for _ in 0..20 {
        m.s.screen = Screen::OnTime;
        m.call(1.0, 1.3, 1, 3);
    }
    assert!(m.s.k == 1 && m.s.ups == 0);
    for _ in 0..3 {
        m.s.screen = Screen::Late;
        m.call(1.0, 0.8, 1, 3);
    }
    assert!(m.s.k == 2 && m.s.ups == 1);
    m.call(2.0, 0.5, 1, 3);
    assert_eq!(m.s.screen, Screen::Unjudged);
}

#[test]
fn at_120_the_unit_is_a_tick_and_the_wait_is_the_same_real_time() {
    let mut m = Sim::default();
    m.s.per_vsync = 2;
    m.run(120, 0.7, 1, 2, 0);
    assert!(m.s.k == 1 && m.s.hold == FSTEP_HOLD0 * 2.0);
    m.run(10, 1.6, 1, 2, 0);
    assert_eq!(m.s.k, 2);
    m.run(60, 4.0, 1, 2, 0);
    assert_eq!(m.s.k, 2);
    m.run(1200, 0.7, 1, 2, 0);
    assert_eq!(m.s.k, 1);
}

fn gpu_sim(gpu: impl Fn(usize, i32) -> f64, cost: impl Fn(usize, i32) -> f64, n: usize) -> Sim {
    let mut m = Sim::default();
    let mut g = GpuSlow::default();
    for i in 0..n {
        let k = if m.s.k != 0 { m.s.k } else { 1 };
        m.slow = g.slowed(gpu(i, k), k, 1, m.s.probe);
        let w = cost(i, k);
        m.run(1, w, 1, 3, 0);
    }
    m
}

#[test]
fn a_gpu_clock_that_follows_the_hold_starts_one_probe() {
    let mut at1 = 0;
    let mut stepped = false;
    let mut m = Sim::default();
    let mut g = GpuSlow::default();
    for i in 0..2000 {
        let w = if i < 600 {
            0.86
        } else if i < 603 {
            1.25
        } else if m.s.k >= 2 {
            1.5
        } else {
            at1 += 1;
            if at1 <= 20 { 1.5 } else { 0.86 }
        };
        stepped |= i >= 603 && m.s.k == 2;
        m.slow = g.slowed(w, if m.s.k != 0 { m.s.k } else { 1 }, 1, m.s.probe);
        m.run(1, w, 1, 3, 0);
    }
    assert!(stepped && m.s.k == 1 && m.s.probes == 1 && m.s.failed == 0);
    assert!(m.paid_back() && m.s.dropped == 0.0);
}

#[test]
fn a_heavier_scene_is_not_a_slow_gpu() {
    let w = |i: usize, _| if i < 600 { 0.86 } else { 1.12 };
    let m = gpu_sim(w, w, 2000);
    assert!(m.s.k == 2 && m.s.probes == 0);
}

#[test]
fn a_gpu_at_three_milliseconds_is_not_called_slow() {
    let gpu = |i: usize, k: i32| if i < 600 || k == 1 { 0.03 } else { 0.17 };
    let cost = |i: usize, _| if i < 600 { 0.5 } else { 1.3 };
    let m = gpu_sim(gpu, cost, 1200);
    assert!(m.s.k == 2 && m.s.probes == 0);
}

#[test]
fn the_gpu_reference_is_kept_per_kmin_and_not_taken_in_a_probe() {
    let mut g = GpuSlow::default();
    for _ in 0..40 {
        assert!(!g.slowed(0.86, 1, 1, 0));
    }
    let r = g.reference;
    for _ in 0..40 {
        g.slowed(1.5, 1, 1, 1);
    }
    assert_eq!(g.reference, r);
    let mut slow = false;
    for _ in 0..40 {
        slow = g.slowed(1.5, 2, 1, 0);
    }
    assert!(slow);
    assert!(!g.slowed(0.5, 1, 4, 0));
}

/// A scene whose cost is 1.3 at hold 2, with a slow report and the profile `work` (the longest hold wait is 120). The
/// first probe runs at the costs that `at1` gives for its frames (counted from 1) and ends when it has failed.
fn failed_probe(at1: impl Fn(i32) -> f64) -> Sim {
    let mut m = Sim::default();
    m.s.work = true;
    m.run(60, 0.5, 1, 3, 0);
    m.run(300, 1.3, 1, 3, 0);
    m.slow = true;
    let (mut n, mut frames) = (0, 0);
    while m.s.failed == 0 && n < 600 {
        let w = if m.s.k == 1 {
            frames += 1;
            at1(frames)
        } else {
            1.3
        };
        m.run(1, w, 1, 3, 0);
        n += 1;
    }
    assert!(m.s.failed == 1 && m.s.probes == 1 && m.s.k == 2);
    m
}

/// Runs a heavy scene (1.4 at hold 1, 1.3 at hold 2) and returns `since` at each start of a probe.
fn heavy_starts(m: &mut Sim, frames: usize) -> Vec<f64> {
    let mut starts = Vec::new();
    for _ in 0..frames {
        let probes = m.s.probes;
        let w = if m.s.k == 1 { 1.4 } else { 1.3 };
        m.run(1, w, 1, 3, 0);
        if m.s.probes != probes {
            starts.push(m.s.since);
        }
    }
    starts
}

const WAIT0: f64 = 2.0 * FSTEP_HOLD_MAX_WORK;

#[test]
fn a_failed_probe_records_the_cost_that_would_hold_kmin() {
    let mut m = failed_probe(|_| 1.4);
    assert!((m.s.retry_below - 1.3 / 1.4).abs() < 0.01 && m.s.retry_k == 2);
    assert_eq!(m.s.retry_hold, WAIT0);
    m.run(110, 1.3, 1, 3, 0);
    assert!(m.s.probes == 1 && !m.s.may_probe());
    m.run(15, 1.3, 1, 3, 0);
    assert!(m.s.probes == 2 && m.s.since >= WAIT0);
}

#[test]
fn each_failed_probe_doubles_the_wait() {
    let mut m = failed_probe(|_| 1.4);
    let starts = heavy_starts(&mut m, 3000);
    assert_eq!(starts[..3], [WAIT0, 2.0 * WAIT0, 4.0 * WAIT0]);
    assert_eq!(m.s.failed, starts.len() as u64 + 1);
}

#[test]
fn the_wait_has_a_maximum() {
    let mut m = failed_probe(|_| 1.4);
    let starts = heavy_starts(&mut m, 9000);
    assert!(starts.len() >= 6);
    assert!(starts.iter().all(|&s| s <= FSTEP_RETRY_MAX));
    assert!(starts[4..].iter().all(|&s| s == FSTEP_RETRY_MAX));
    assert_eq!(m.s.retry_hold, FSTEP_RETRY_MAX);
}

#[test]
fn a_cheaper_scene_at_the_longer_hold_starts_a_probe_before_the_wait_is_over() {
    let mut m = failed_probe(|_| 1.4);
    m.run(70, 0.9, 1, 3, 0);
    assert!(m.s.probes == 2 && m.s.k == 1);
    assert!(m.s.retry_hold > 0.0 && m.s.since < m.s.retry_hold);
}

#[test]
fn a_lighter_scene_behind_a_lowered_clock_is_found_after_the_wait() {
    let mut m = failed_probe(|_| 1.4);
    for _ in 0..400 {
        let w = if m.s.k == 1 { 0.6 } else { 1.3 };
        m.run(1, w, 1, 3, 0);
    }
    assert!(m.s.probes == 2 && m.s.failed == 1 && m.s.k == 1 && m.s.retry_hold == 0.0);
}

#[test]
fn a_probe_that_holds_ends_the_record() {
    let mut m = failed_probe(|_| 1.4);
    m.run(200, 0.9, 1, 3, 0);
    assert!(m.s.k == 1 && m.s.trial == 0);
    assert!(m.s.retry_below == 0.0 && m.s.retry_hold == 0.0);
}

#[test]
fn a_change_of_kmin_ends_the_record() {
    let mut m = failed_probe(|_| 1.4);
    assert!(m.s.retry_below > 0.0 && m.s.retry_hold > 0.0);
    m.run(1, 1.3, 2, 3, 0);
    assert!(m.s.retry_below == 0.0 && m.s.retry_hold == 0.0);
}

#[test]
fn a_failed_probe_without_a_cost_at_the_longer_hold_still_waits() {
    let mut m = Sim::default();
    m.s.work = true;
    m.run(600, 0.86, 1, 3, 0);
    m.run(600, 1.5, 2, 2, 0);
    m.run(1, 1.5, 1, 3, 0);
    assert!(m.s.probes == 1 && m.s.plong == 0.0);
    let mut frames = 0;
    while m.s.k == 1 && frames < 100 {
        m.run(1, 1.3, 1, 3, 0);
        frames += 1;
    }
    assert!(m.s.failed == 1 && m.s.retry_below == 0.0 && m.s.retry_hold == WAIT0);
    assert!(!m.s.may_probe());
    m.run(125, 1.3, 1, 3, 0);
    assert!(m.s.may_probe());
}

#[test]
fn a_probe_that_mostly_fit_keeps_the_cost_at_the_longer_hold() {
    let mut m = failed_probe(|f| if f > 40 { 1.4 } else { 0.6 });
    assert!((m.s.retry_below - 1.3).abs() < 1e-9 && m.s.retry_k == 2);
    m.run(40, 1.3, 1, 3, 0);
    assert!(m.s.may_probe());
}

#[test]
fn the_probe_cost_is_the_median_of_its_frames() {
    let mut s = FrameStep {
        plast: [1.0; 16],
        pcount: 16,
        ..FrameStep::default()
    };
    s.plast[3] = 50.0;
    assert_eq!(s.probe_cost(), 1.0);
    s.plast[..3].copy_from_slice(&[1.0, 9.0, 1.2]);
    s.pcount = 3;
    assert_eq!(s.probe_cost(), 1.2);
}
