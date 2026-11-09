use super::*;
use crate::frame::{CmdStream, DrawState, Span};
use crate::marks::{Mark, MarkOp, RENDER_IF_NOT_EQUAL};
use crate::rendercond::RenderConds;

const Q: usize = 1;

fn draw() -> Cmd {
    Cmd::Draw {
        state: DrawState::default(),
        reads: Span::default(),
    }
}

fn stream(n: usize, marks: &[(u32, MarkOp)]) -> CmdStream {
    let mut s = CmdStream::new();
    s.cmds.extend((0..n).map(|_| draw()));
    s.marks
        .extend(marks.iter().map(|&(at, op)| Mark { at, op }));
    s
}

fn run(rc: &mut RenderConds, f: &mut Frame, queue: usize, s: &CmdStream) {
    let base = f.cmds.len() as u32;
    f.append(s);
    rc.apply(queue, base, &s.marks, f, |_| None);
}

fn report(addr: u64) -> MarkOp {
    MarkOp::ReportCounter {
        addr,
        kind: SAMPLES_PASSED,
    }
}

fn reset() -> MarkOp {
    MarkOp::ResetCounter {
        kind: SAMPLES_PASSED,
    }
}

fn sums(f: &Frame, i: usize) -> Vec<u32> {
    f.counters.sums_of(&f.counters.writes[i]).to_vec()
}

#[test]
fn an_occlusion_test_sums_the_slot_of_its_boxes() {
    let (mut rc, mut f) = (RenderConds::new(), Frame::new());
    // Draw 0 renders before the test, draws 1 and 2 are the boxes, draw 3 comes after it.
    let s = stream(4, &[(1, reset()), (1, report(0x100)), (3, report(0x110))]);
    run(&mut rc, &mut f, Q, &s);
    assert_eq!(f.counters.writes.len(), 2);
    assert!(sums(&f, 0).is_empty());
    let boxes = sums(&f, 1);
    assert_eq!(boxes.len(), 1);
    assert_eq!(f.counters.slot_of(1), Some(boxes[0]));
    assert_eq!(f.counters.slot_of(2), Some(boxes[0]));
    assert_ne!(f.counters.slot_of(3), Some(boxes[0]));
    assert!(f.counters.writes[0].seq < f.counters.writes[1].seq);
    assert!(f.counters.landed.is_some());
}

#[test]
fn a_report_sums_every_slot_since_the_reset_of_its_counter() {
    let (mut rc, mut f) = (RenderConds::new(), Frame::new());
    let s = stream(3, &[(0, reset()), (1, report(0x100)), (2, report(0x110))]);
    run(&mut rc, &mut f, Q, &s);
    let (first, second) = (sums(&f, 0), sums(&f, 1));
    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 2);
    assert_eq!(second[0], first[0]);
}

#[test]
fn the_slots_of_a_queue_skip_the_commands_of_another_queue() {
    let (mut rc, mut f) = (RenderConds::new(), Frame::new());
    run(&mut rc, &mut f, Q, &stream(1, &[(0, reset())]));
    run(&mut rc, &mut f, Q + 1, &stream(2, &[]));
    run(&mut rc, &mut f, Q, &stream(1, &[(1, report(0x100))]));
    let slot = sums(&f, 0);
    assert_eq!(slot.len(), 1);
    assert_eq!(f.counters.slot_of(0), Some(slot[0]));
    assert_eq!(f.counters.slot_of(1), None);
    assert_eq!(f.counters.slot_of(2), None);
    assert_eq!(f.counters.slot_of(3), Some(slot[0]));
}

#[test]
fn a_new_frame_starts_without_slots() {
    let (mut rc, mut f) = (RenderConds::new(), Frame::new());
    run(&mut rc, &mut f, Q, &stream(2, &[(0, reset())]));
    rc.flushed();
    f.clear();
    run(&mut rc, &mut f, Q, &stream(1, &[(1, report(0x100))]));
    let slot = sums(&f, 0);
    assert_eq!(slot, [0]);
    assert_eq!(f.counters.slots, 1);
}

#[test]
fn a_counter_the_host_cannot_count_writes_nothing() {
    let (mut rc, mut f) = (RenderConds::new(), Frame::new());
    let fragment_invocations = MarkOp::ReportCounter {
        addr: 0x100,
        kind: 8,
    };
    run(&mut rc, &mut f, Q, &stream(1, &[(1, fragment_invocations)]));
    assert!(f.counters.writes.is_empty());
    let st = rc.take_stats();
    assert_eq!((st.counters, st.emulated, st.kinds[8]), (1, 0, 1));
}

/// The two reports of an occlusion test, and a condition on them before the reports (`cond_first`) or after them.
fn occlusion(rc: &mut RenderConds, f: &mut Frame, cond_first: bool) {
    let cond = MarkOp::RenderIf {
        mode: RENDER_IF_NOT_EQUAL,
        addr: 0x100,
    };
    let at = if cond_first { 0 } else { 2 };
    let mut marks = vec![(0, reset()), (0, report(0x100)), (1, report(0x110))];
    marks.insert(if cond_first { 0 } else { 3 }, (at, cond));
    run(rc, f, Q, &stream(3, &marks));
}

/// Lands the reports of the frame with `samples` samples between the two.
fn land(rc: &RenderConds, f: &Frame, samples: u64) {
    for w in &f.counters.writes {
        rc.landed()
            .land(w.addr, w.place, u64::from(w.addr == 0x110) * samples, w.seq);
    }
}

fn next_frame(rc: &mut RenderConds, f: &mut Frame) {
    rc.flushed();
    f.clear();
}

#[test]
fn a_condition_reads_the_counters_of_the_reports_it_names_once_they_landed() {
    let (mut rc, mut f) = (RenderConds::new(), Frame::new());
    occlusion(&mut rc, &mut f, true);
    // Nothing landed yet: the condition renders.
    assert!(!f.skips(2));
    assert_eq!(rc.take_stats().unknown, 1);
    land(&rc, &f, 0);
    next_frame(&mut rc, &mut f);
    // The boxes of the frame before passed no sample: the object is left out.
    occlusion(&mut rc, &mut f, true);
    assert!(f.skips(2));
    let st = rc.take_stats();
    assert_eq!((st.false_, st.landed, st.stale), (1, 1, 0));
    land(&rc, &f, 40);
    next_frame(&mut rc, &mut f);
    occlusion(&mut rc, &mut f, true);
    assert!(!f.skips(2));
}

#[test]
fn a_condition_on_reports_of_its_own_frame_reads_the_last_landed_ones() {
    let (mut rc, mut f) = (RenderConds::new(), Frame::new());
    occlusion(&mut rc, &mut f, false);
    assert!(!f.skips(2));
    land(&rc, &f, 40);
    next_frame(&mut rc, &mut f);
    occlusion(&mut rc, &mut f, false);
    assert!(!f.skips(2));
    let st = rc.take_stats();
    assert_eq!((st.true_, st.landed, st.stale), (1, 0, 1));
    land(&rc, &f, 0);
    next_frame(&mut rc, &mut f);
    occlusion(&mut rc, &mut f, false);
    assert!(f.skips(2));
}

#[test]
fn landed_keeps_the_newest_report() {
    let l = Landed::default();
    l.land(0x100, (1, 0), 5, 2);
    l.land(0x100, (1, 0), 9, 1);
    assert_eq!(l.get(0x100), Some((5, 2)));
    assert_eq!(l.at_place((1, 0)), Some((5, 2)));
}

#[test]
fn primitives_follow_the_topology() {
    assert_eq!(primitives(4, 9), Some(3));
    assert_eq!(primitives(5, 9), Some(7));
    assert_eq!(primitives(1, 4), Some(2));
    assert_eq!(primitives(14, 9), None);
}

#[test]
fn counted_draws_leave_out_what_a_false_condition_leaves_out() {
    let (mut rc, mut f) = (RenderConds::new(), Frame::new());
    let s = stream(
        3,
        &[
            (0, reset()),
            (1, MarkOp::RenderEnable(false)),
            (2, MarkOp::RenderEnable(true)),
            (3, report(0x100)),
        ],
    );
    run(&mut rc, &mut f, Q, &s);
    let draws: Vec<u32> = counted_draws(&f).map(|(i, _)| i).collect();
    assert_eq!(draws, [0, 2]);
}

#[test]
fn a_report_right_after_a_reset_is_zero_now_and_one_after_a_draw_is_not() {
    let (mut rc, mut f) = (RenderConds::new(), Frame::new());
    let cond = MarkOp::RenderIf {
        mode: RENDER_IF_NOT_EQUAL,
        addr: 0x100,
    };
    // Begin and end report with no draw between: both zero, the condition is false at once.
    let s = stream(
        1,
        &[
            (0, reset()),
            (0, report(0x100)),
            (0, report(0x110)),
            (0, cond),
        ],
    );
    run(&mut rc, &mut f, Q, &s);
    assert!(f.skips(0));
    assert_eq!(rc.take_stats().zero, 2);
    // With a draw between, the end report waits for the GPU.
    let s = stream(1, &[(0, reset()), (0, report(0x100)), (1, report(0x110))]);
    run(&mut rc, &mut f, Q, &s);
    assert_eq!(rc.take_stats().zero, 1);
}

#[test]
fn a_report_at_a_new_address_reads_the_report_at_its_place_in_the_frame_before() {
    let (mut rc, mut f) = (RenderConds::new(), Frame::new());
    // The game moves its query memory every frame: frame 1 at 0x100, frame 2 at 0x200.
    let frame = |rc: &mut RenderConds, f: &mut Frame, at: u64| {
        let cond = MarkOp::RenderIf {
            mode: RENDER_IF_NOT_EQUAL,
            addr: at,
        };
        let s = stream(
            2,
            &[
                (0, reset()),
                (0, report(at)),
                (1, report(at + 16)),
                (1, cond),
            ],
        );
        run(rc, f, Q, &s);
    };
    frame(&mut rc, &mut f, 0x100);
    for w in &f.counters.writes {
        rc.landed().land(w.addr, w.place, 0, w.seq);
    }
    next_frame(&mut rc, &mut f);
    frame(&mut rc, &mut f, 0x200);
    assert!(f.skips(1));
    assert_eq!(rc.take_stats().placed, 1);
}
