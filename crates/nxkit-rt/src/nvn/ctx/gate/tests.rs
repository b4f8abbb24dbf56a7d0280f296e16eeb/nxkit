use super::*;
use nxkit_nvn::marks::{EVENT_SIGNAL_ADD, EVENT_WAIT_EQUAL, EVENT_WAIT_GEQUAL_WRAP};

/// Guest words in a map. Event `e` has its counter at `e + 0x100`.
#[derive(Default)]
struct Mem(HashMap<u64, u64>);

impl Mem {
    fn event(mut self, e: u64, value: u32) -> Self {
        self.0.insert(e + EVENT_COUNTER, e + 0x100);
        self.0.insert(e + 0x100, u64::from(value));
        self
    }
}

impl Words for Mem {
    fn u32(&self, addr: u64) -> Option<u32> {
        self.0.get(&addr).map(|v| *v as u32)
    }
    fn u64(&self, addr: u64) -> Option<u64> {
        self.0.get(&addr).copied()
    }
}

const E: u64 = 0x1000;
const QA: u64 = 0xa;
const QB: u64 = 0xb;

fn wait(value: u32) -> MarkOp {
    MarkOp::WaitEvent {
        event: E,
        mode: EVENT_WAIT_GEQUAL_WRAP,
        value,
    }
}

fn signal(value: u32) -> MarkOp {
    MarkOp::SignalEvent {
        event: E,
        mode: 0,
        value,
    }
}

fn submit() -> Op {
    Op::Submit {
        handles: vec![CommandHandle(1)],
        origin: false,
    }
}

#[test]
fn a_wait_for_a_counter_that_memory_has_reached_is_satisfied() {
    let (g, mem) = (Gate::new(true), Mem::default().event(E, 5));
    assert_eq!(g.blocker(QA, &[wait(5)], &mem), None);
    assert_eq!(g.blocker(QA, &[wait(6)], &mem), Some(wait(6)));
    let eq = MarkOp::WaitEvent {
        event: E,
        mode: EVENT_WAIT_EQUAL,
        value: 4,
    };
    assert_eq!(g.blocker(QA, &[eq], &mem), Some(eq));
}

#[test]
fn a_gpu_signal_that_ran_satisfies_a_later_wait_without_touching_memory() {
    let (mut g, mem) = (Gate::new(true), Mem::default().event(E, 0));
    assert!(g.blocker(QA, &[wait(1)], &mem).is_some());
    g.ran(&[signal(1)], &mem);
    assert_eq!(g.blocker(QA, &[wait(1)], &mem), None);
    assert_eq!(mem.u32(E + 0x100), Some(0));
    // The CPU wrote the counter: memory is the truth again.
    g.cpu_signalled(E + 0x100);
    assert!(g.blocker(QA, &[wait(1)], &mem).is_some());
}

#[test]
fn a_signal_in_held_work_counts_only_when_the_work_ran() {
    let (mut g, mem) = (Gate::new(true), Mem::default().event(E, 0));
    let held = vec![wait(7), signal(1)];
    g.push(QB, submit());
    g.held[0].marks = Some(held.clone());
    assert_eq!(g.blocker(QB, &held, &mem), Some(wait(7)));
    // Held, its signal is not in the counter (and the backend never saw its work), but a wait of another queue knows
    // that held work signals the event.
    assert_eq!(g.event_value(E, &mem), Some(0));
    assert!(g.signalled_by_held(E + 0x100, QA, &mem));
    let _ = g.held.pop_front();
    g.ran(&held, &mem);
    assert_eq!(g.event_value(E, &mem), Some(1));
    assert_eq!(
        mem.u32(E + 0x100),
        Some(0),
        "guest memory gets it when the GPU finished the work"
    );
}

#[test]
fn a_signal_earlier_in_the_same_work_counts_and_an_add_adds() {
    let (g, mem) = (Gate::new(true), Mem::default().event(E, 3));
    let add = MarkOp::SignalEvent {
        event: E,
        mode: EVENT_SIGNAL_ADD,
        value: 2,
    };
    assert_eq!(g.blocker(QA, &[add, wait(5)], &mem), None);
    assert!(g.blocker(QA, &[wait(5), add], &mem).is_some());
}

#[test]
fn an_event_without_a_counter_never_holds() {
    let g = Gate::new(true);
    assert_eq!(g.blocker(QA, &[wait(9)], &Mem::default()), None);
}

#[test]
fn a_sync_wait_holds_only_while_held_work_of_another_queue_fences_it() {
    let mut g = Gate::new(true);
    let mem = Mem::default();
    let w = MarkOp::WaitSync { sync: 0x50 };
    assert_eq!(g.blocker(QA, &[w], &mem), None);
    g.push(QB, Op::Fence { sync: 0x50 });
    assert_eq!(g.blocker(QA, &[w], &mem), Some(w));
    // The queue's own later fence is not waited for.
    assert_eq!(g.blocker(QB, &[w], &mem), None);
}

#[test]
fn the_heads_are_the_oldest_op_of_each_queue_in_hold_order() {
    let mut g = Gate::new(true);
    g.push(QB, submit());
    g.push(QA, submit());
    g.push(QB, Op::Wait { sync: 1 });
    g.push(QA, submit());
    assert_eq!(g.heads(), [0, 1]);
    assert!(g.holds(QA) && g.holds(QB) && !g.holds(0xc));
    assert_eq!(g.held[2].marks, Some(vec![MarkOp::WaitSync { sync: 1 }]));
    assert_eq!(g.held[0].marks, None);
}

#[test]
fn the_work_that_ran_counts_its_waits_signals_and_passes_the_held_ops() {
    let mut g = Gate::new(true);
    let mem = Mem::default().event(E, 0);
    g.push(QA, submit());
    g.ran(&[signal(1), MarkOp::FenceSync { sync: 2 }, wait(1)], &mem);
    assert_eq!((g.stats.signals, g.stats.fences, g.stats.waits), (1, 1, 1));
    assert_eq!(g.held[0].passed, 1);
}

#[test]
fn a_flush_forces_first_the_wait_that_no_held_work_can_satisfy() {
    const F: u64 = 0x2000;
    let mem = Mem::default().event(E, 0).event(F, 0);
    let mut g = Gate::new(true);
    // QA waits for the GPU signal of QB; QB waits for a CPU signal of F that comes after the present.
    let on_f = MarkOp::WaitEvent {
        event: F,
        mode: EVENT_WAIT_EQUAL,
        value: 1,
    };
    g.push(QA, submit());
    g.push(QB, submit());
    g.held[0].marks = Some(vec![wait(1)]);
    g.held[1].marks = Some(vec![on_f, signal(1)]);
    let blockers: Vec<_> = g
        .heads()
        .into_iter()
        .map(|i| {
            (
                i,
                g.blocker(g.held[i].queue, g.held[i].marks.as_ref().unwrap(), &mem),
            )
        })
        .collect();
    assert_eq!(blockers, [(0, Some(wait(1))), (1, Some(on_f))]);
    assert!(g.signalled_by_held(E + 0x100, QA, &mem));
    assert_eq!(g.force_pick(&blockers, &mem), 1);
    // Once QB ran, QA may run.
    let qb = g.held.remove(1).unwrap();
    g.ran(qb.marks.as_ref().unwrap(), &mem);
    assert_eq!(g.blocker(QA, &[wait(1)], &mem), None);
}

#[test]
fn a_flush_forces_the_oldest_when_every_wait_waits_for_held_work() {
    let mem = Mem::default().event(E, 0);
    let mut g = Gate::new(true);
    g.push(QA, submit());
    g.push(QB, submit());
    g.held[0].marks = Some(vec![wait(1), signal(2)]);
    g.held[1].marks = Some(vec![wait(2), signal(1)]);
    let blockers = vec![(0, Some(wait(1))), (1, Some(wait(2)))];
    assert_eq!(g.force_pick(&blockers, &mem), 0);
}
