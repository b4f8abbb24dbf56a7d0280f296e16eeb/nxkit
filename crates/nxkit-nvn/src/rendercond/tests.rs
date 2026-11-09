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
