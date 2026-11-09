use super::*;
use crate::frame::{CmdStream, DrawState, Span};

const Q: usize = 1;

fn draw() -> Cmd {
    Cmd::Draw {
        state: DrawState::default(),
        reads: Span::default(),
    }
}

/// A stream of `n` draws with the marks at the given commands.
fn stream(n: usize, marks: &[(u32, MarkOp)]) -> CmdStream {
    let mut s = CmdStream::new();
    s.cmds.extend((0..n).map(|_| draw()));
    s.marks
        .extend(marks.iter().map(|&(at, op)| Mark { at, op }));
    s
}

fn run(rc: &mut RenderConds, f: &mut Frame, s: &CmdStream, mem: &[(u64, u64)]) {
    let base = f.cmds.len() as u32;
    f.append(s);
    rc.apply(Q, base, &s.marks, f, |a| {
        mem.iter().find(|(k, _)| *k == a).map(|(_, v)| *v)
    });
}

fn skipped(f: &Frame) -> Vec<u32> {
    (0..f.cmds.len() as u32).filter(|i| f.skips(*i)).collect()
}

#[test]
fn render_enable_false_leaves_out_the_draws_until_it_is_true_again() {
    let (mut rc, mut f) = (RenderConds::new(), Frame::new());
    let s = stream(
        4,
        &[
            (1, MarkOp::RenderEnable(false)),
            (3, MarkOp::RenderEnable(true)),
        ],
    );
    run(&mut rc, &mut f, &s, &[]);
    assert_eq!(skipped(&f), [1, 2]);
}

#[test]
fn the_condition_of_a_queue_lasts_into_its_next_submit() {
    let (mut rc, mut f) = (RenderConds::new(), Frame::new());
    run(
        &mut rc,
        &mut f,
        &stream(1, &[(1, MarkOp::RenderEnable(false))]),
        &[],
    );
    run(&mut rc, &mut f, &stream(2, &[]), &[]);
    assert_eq!(skipped(&f), [1, 2]);
    // Another queue renders.
    let base = f.cmds.len() as u32;
    let s = stream(1, &[]);
    f.append(&s);
    rc.apply(Q + 1, base, &s.marks, &mut f, |_| None);
    assert!(!f.skips(3));
}

#[test]
fn a_conditional_render_compares_the_two_counters_in_guest_memory() {
    let mem = [(0x100, 5), (0x110, 5), (0x200, 5), (0x210, 6)];
    let cases = [
        (RENDER_IF_EQUAL, 0x100, false),
        (RENDER_IF_EQUAL, 0x200, true),
        (RENDER_IF_NOT_EQUAL, 0x100, true),
        (RENDER_IF_NOT_EQUAL, 0x200, false),
    ];
    for (mode, addr, left_out) in cases {
        let (mut rc, mut f) = (RenderConds::new(), Frame::new());
        run(
            &mut rc,
            &mut f,
            &stream(2, &[(1, MarkOp::RenderIf { mode, addr })]),
            &mem,
        );
        assert_eq!(f.skips(1), left_out, "mode {mode} addr {addr:#x}");
        assert!(!f.skips(0));
    }
}

#[test]
fn a_report_value_of_an_earlier_command_is_seen_before_it_reaches_memory() {
    let (mut rc, mut f) = (RenderConds::new(), Frame::new());
    let s = stream(
        2,
        &[
            (
                0,
                MarkOp::ReportValue {
                    addr: 0x100,
                    value: 1,
                },
            ),
            (
                0,
                MarkOp::ReportValue {
                    addr: 0x110,
                    value: 2,
                },
            ),
            (
                1,
                MarkOp::RenderIf {
                    mode: RENDER_IF_EQUAL,
                    addr: 0x100,
                },
            ),
        ],
    );
    // Guest memory still says equal; the reports say not.
    run(&mut rc, &mut f, &s, &[(0x100, 0), (0x110, 0)]);
    assert_eq!(skipped(&f), [1]);
    assert_eq!(rc.take_stats().false_, 1);
}

#[test]
fn a_counter_only_the_gpu_knows_renders() {
    let (mut rc, mut f) = (RenderConds::new(), Frame::new());
    let s = stream(
        2,
        &[
            (
                0,
                MarkOp::ReportCounter {
                    addr: 0x100,
                    kind: 1,
                },
            ),
            (
                1,
                MarkOp::RenderIf {
                    mode: RENDER_IF_NOT_EQUAL,
                    addr: 0x100,
                },
            ),
        ],
    );
    run(&mut rc, &mut f, &s, &[(0x100, 0), (0x110, 0)]);
    assert!(skipped(&f).is_empty());
    assert_eq!(rc.take_stats().unknown, 1);
}

#[test]
fn a_known_value_ages_out_to_guest_memory() {
    let (mut rc, mut f) = (RenderConds::new(), Frame::new());
    let report = (
        0,
        MarkOp::ReportValue {
            addr: 0x100,
            value: 1,
        },
    );
    run(&mut rc, &mut f, &stream(0, &[report]), &[]);
    for _ in 0..KEEP_FLUSHES {
        rc.flushed();
    }
    let cond = MarkOp::RenderIf {
        mode: RENDER_IF_EQUAL,
        addr: 0x100,
    };
    f.clear();
    run(
        &mut rc,
        &mut f,
        &stream(1, &[(0, cond)]),
        &[(0x100, 7), (0x110, 7)],
    );
    assert!(!f.skips(0));
}

#[test]
fn only_conditional_commands_are_left_out_and_ranges_join() {
    let mut f = Frame::new();
    f.cmds.extend([
        draw(),
        Cmd::Barrier(1),
        Cmd::ClearDepthStencil { full: true },
    ]);
    f.disable(0..1);
    f.disable(1..3);
    assert_eq!(f.disabled.len(), 1);
    assert_eq!(f.disabled[0], 0..3);
    assert_eq!(skipped(&f), [0, 2]);
    assert_eq!(disabled_counts(&f), [1, 1, 0, 0]);
}

#[test]
fn marks_of_a_spliced_stream_move_behind_the_caller() {
    let mut caller = stream(2, &[(1, MarkOp::RenderEnable(false))]);
    let callee = stream(1, &[(0, MarkOp::RenderEnable(true))]);
    caller.splice(&callee);
    assert_eq!(caller.marks[1].at, 2);
}

fn mem_read(mem: &'static [(u64, u64)]) -> impl Fn(u64) -> Option<u64> {
    move |a| mem.iter().find(|(k, _)| *k == a).map(|(_, v)| *v)
}

const FLAG_FALSE: &[(u64, u64)] = &[(0x100, 5), (0x110, 6)];

#[test]
fn without_a_switch_the_decision_is_the_natural_one() {
    use force::{Source as S, decide};
    for (natural, source) in [
        (Some(true), S::Counter),
        (Some(false), S::Counter),
        (Some(false), S::Memory),
        (None, S::Unknown),
    ] {
        let d = decide(natural, source, 0);
        assert_eq!(d.render, natural.unwrap_or(true));
        assert_eq!(d.forced, 0);
    }
}

#[test]
fn each_switch_alone_forces_its_own_case() {
    use force::{RCALL, RCFLAG, RCUNK, Source as S, decide};
    // rcflag: memory operands render, nothing else changes.
    assert!(decide(Some(false), S::Memory, RCFLAG).render);
    assert_eq!(decide(Some(false), S::Memory, RCFLAG).forced, RCFLAG);
    assert!(!decide(Some(false), S::Counter, RCFLAG).render);
    assert_eq!(decide(Some(false), S::Counter, RCFLAG).forced, 0);
    // rcunk: an unknown result is left out, a known one is not touched.
    assert!(!decide(None, S::Unknown, RCUNK).render);
    assert_eq!(decide(None, S::Unknown, RCUNK).forced, RCUNK);
    assert!(decide(Some(true), S::Counter, RCUNK).render);
    assert!(!decide(Some(false), S::Memory, RCUNK).render);
    // rcall: everything renders.
    for (natural, source) in [
        (Some(false), S::Counter),
        (Some(false), S::Memory),
        (None, S::Unknown),
    ] {
        let d = decide(natural, source, RCALL);
        assert!(d.render);
        assert_eq!(d.forced, RCALL);
    }
    // The switches that act elsewhere decide nothing.
    for bit in [force::NOSTALE, force::ONLY_3D] {
        assert!(!decide(Some(false), S::Counter, bit).render);
        assert!(decide(None, S::Unknown, bit).render);
    }
    // rcall wins over rcunk.
    assert!(decide(None, S::Unknown, RCALL | RCUNK).render);
}

#[test]
fn two_words_of_guest_memory_are_a_memory_condition() {
    let mut rc = RenderConds::new();
    let i = rc.evaluate_with(RENDER_IF_EQUAL, 0x100, &mem_read(FLAG_FALSE), 0);
    assert_eq!(i.reads, [Read::Memory(5), Read::Memory(6)]);
    assert_eq!(i.source, force::Source::Memory);
    assert!(!i.decision.render);
    assert_eq!(i.decision_name(), "skip");
    assert_eq!(rc.take_stats().forced, [0; 3]);
}

#[test]
fn a_known_report_value_is_not_memory() {
    let mut rc = RenderConds::new();
    let known = |value| Report::Known { value, flush: 0 };
    rc.report(0x100, known(5));
    rc.report(0x110, known(6));
    let i = rc.evaluate_with(RENDER_IF_EQUAL, 0x100, &mem_read(&[]), force::RCFLAG);
    assert_eq!(i.reads, [Read::Known(5), Read::Known(6)]);
    assert_eq!(i.source, force::Source::Counter);
    assert!(!i.decision.render);
    assert_eq!(i.decision.forced, 0);
    // One report and one word of memory is not a memory condition either.
    let i = rc.evaluate_with(
        RENDER_IF_EQUAL,
        0x108,
        &mem_read(&[(0x108, 1), (0x118, 2)]),
        0,
    );
    assert_eq!(i.source, force::Source::Memory);
    rc.report(0x200, known(1));
    let i = rc.evaluate_with(RENDER_IF_EQUAL, 0x200, &mem_read(&[(0x210, 2)]), 0);
    assert_eq!(i.reads, [Read::Known(1), Read::Memory(2)]);
    assert_eq!(i.source, force::Source::Counter);
}

#[test]
fn a_forced_decision_is_counted_and_named() {
    let mut rc = RenderConds::new();
    let i = rc.evaluate_with(RENDER_IF_EQUAL, 0x100, &mem_read(FLAG_FALSE), force::RCFLAG);
    assert!(i.decision.render);
    assert_eq!(i.decision_name(), "forced-rcflag-render");
    // An unknown counter is left out with rcunk.
    rc.report(0x300, Report::Unknown);
    let i = rc.evaluate_with(RENDER_IF_EQUAL, 0x300, &mem_read(&[]), force::RCUNK);
    assert_eq!(i.source, force::Source::Unknown);
    assert_eq!(i.decision_name(), "forced-rcunk-skip");
    let i = rc.evaluate_with(RENDER_IF_EQUAL, 0x100, &mem_read(FLAG_FALSE), force::RCALL);
    assert_eq!(i.decision_name(), "forced-rcall-render");
    assert_eq!(rc.take_stats().forced, [1, 1, 1]);
}

#[test]
fn a_decision_with_no_switch_is_not_counted_as_forced() {
    let mut rc = RenderConds::new();
    rc.report(0x300, Report::Unknown);
    let i = rc.evaluate_with(RENDER_IF_EQUAL, 0x300, &mem_read(&[]), 0);
    assert!(i.decision.render);
    assert_eq!(i.source, force::Source::Unknown);
    assert_eq!(rc.take_stats().forced, [0; 3]);
}

#[test]
fn a_run_under_a_condition_is_recorded_in_the_frame() {
    let mut rc = RenderConds::new();
    let info = rc.evaluate_with(RENDER_IF_EQUAL, 0x100, &mem_read(FLAG_FALSE), 0);
    let mut f = Frame::new();
    RenderConds::cover(&mut f, 2..5, Some(info));
    // An empty run and a run under no condition are not recorded.
    RenderConds::cover(&mut f, 5..5, Some(info));
    RenderConds::cover(&mut f, 5..9, None);
    assert_eq!(f.gates.len(), 1);
    assert_eq!(f.gates[0].range, 2..5);
    assert_eq!(f.gates[0].info.addr, 0x100);
    f.clear();
    assert!(f.gates.is_empty());
}
