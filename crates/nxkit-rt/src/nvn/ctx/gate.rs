//! The order of the queues: a queue whose work waits for a signal of another queue is held until the signal is in the
//! frame.
//!
//! The host replays the work of every queue in one frame, in the order of the submits. On the console a queue that
//! waits for an event (`nvnCommandBufferWaitEvent`) stops until another queue signals it, so work submitted before its
//! producer still runs after it. The gate keeps that order: a submit with a wait that is not satisfied yet is held,
//! with everything the same queue submits after it, and runs when the signal ran. A flush (present, CPU wait) runs
//! what is still held, in the order it came, so no work is lost and no CPU wait waits for ever.
//!
//! What satisfies a wait:
//! - `WaitEvent`: the counter of the event, as the GPU signals of the work that ran left it ([`Gate::ran`]) or as guest
//!   memory has it (the CPU signals write it), passes the compare of the wait. A signal earlier in the same submit
//!   counts.
//! - `WaitSync` (in a recording or `nvnQueueWaitSync`): no held work of another queue fences the sync. A fence that
//!   was submitted already is in the frame before the wait. A fence that is not submitted yet is not waited for: NVN
//!   binds the wait to the fence the sync has when the wait is made.
//!
//! What a flush runs first ([`Gate::force_pick`]): the oldest held op whose wait no held work can satisfy (a wait for
//! a CPU signal that has not come yet, the game signals it after the present), then what that releases. Running the
//! held work strictly oldest first ran a queue that waits for the GPU signal of another held queue before that queue,
//! against the order the console keeps.
//!
//! `NXKIT_QUEUE_ORDER=0` holds nothing; the waits that would have held are still counted.

use std::collections::{HashMap, VecDeque};

use nxkit_nvn::cmdbuf::CommandHandle;
use nxkit_nvn::marks::{MarkOp, event_signalled, event_wait_done};

/// The offset of the counter address in an `NVNevent` (see the event group).
pub(crate) const EVENT_COUNTER: u64 = 8;

/// Reads guest words for the gate.
pub(crate) trait Words {
    /// The `u32` at `addr`.
    fn u32(&self, addr: u64) -> Option<u32>;
    /// The `u64` at `addr`.
    fn u64(&self, addr: u64) -> Option<u64>;
}

/// Work of a queue that the gate may hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    /// `nvnQueueSubmitCommands`.
    Submit {
        handles: Vec<CommandHandle>,
        origin: bool,
    },
    /// `nvnQueueFenceSync` of a held queue: it comes after the held work.
    Fence { sync: u64 },
    /// `nvnQueueWaitSync` on a sync that held work fences.
    Wait { sync: u64 },
}

/// Held work.
#[derive(Debug)]
pub(crate) struct Held {
    pub(crate) queue: u64,
    pub(crate) op: Op,
    /// The waits and signals of the op, read when it is first the oldest of its queue.
    pub(crate) marks: Option<Vec<MarkOp>>,
    /// Whether the hold was counted.
    pub(crate) counted: bool,
    /// The ops of other queues that ran while it was held.
    pub(crate) passed: u32,
}

/// The counters of `NXKIT_DIAG=queues` since the last report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct GateStats {
    /// Submits whose waits and signals were read.
    pub(crate) checked: u32,
    /// Waits in the work that ran (event and sync).
    pub(crate) waits: u32,
    /// Ops that were held because a wait was not satisfied.
    pub(crate) held: u32,
    /// Waits that would have held with `NXKIT_QUEUE_ORDER=0`.
    pub(crate) would_hold: u32,
    /// Held ops that ran when their signal ran.
    pub(crate) released: u32,
    /// Held ops that a flush ran without their signal.
    pub(crate) forced: u32,
    /// GPU event signals and fences in the work that ran.
    pub(crate) signals: u32,
    pub(crate) fences: u32,
    /// CPU event signals.
    pub(crate) cpu_signals: u32,
}

impl GateStats {
    /// True when nothing was counted.
    pub(crate) fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The held work and the event counters as the work that ran left them.
#[derive(Debug, Default)]
pub(crate) struct Gate {
    pub(crate) held: VecDeque<Held>,
    /// The counter of each event (by the address of its word) after the GPU signals that ran. Guest memory has the
    /// counter when it is not here.
    events: HashMap<u64, u32>,
    pub(crate) stats: GateStats,
    /// Whether waits hold work (`NXKIT_QUEUE_ORDER` is not `0`).
    pub(crate) on: bool,
}

impl Gate {
    pub(crate) fn new(on: bool) -> Self {
        Self {
            on,
            ..Self::default()
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    /// True when `queue` has held work: what it does next comes after it.
    pub(crate) fn holds(&self, queue: u64) -> bool {
        self.held.iter().any(|h| h.queue == queue)
    }

    pub(crate) fn push(&mut self, queue: u64, op: Op) {
        let marks = match op {
            Op::Fence { sync } => Some(vec![MarkOp::FenceSync { sync }]),
            Op::Wait { sync } => Some(vec![MarkOp::WaitSync { sync }]),
            Op::Submit { .. } => None,
        };
        self.held.push_back(Held {
            queue,
            op,
            marks,
            counted: false,
            passed: 0,
        });
    }

    /// The index of the oldest held op of each queue, in hold order.
    pub(crate) fn heads(&self) -> Vec<usize> {
        let mut seen = Vec::new();
        let mut out = Vec::new();
        for (i, h) in self.held.iter().enumerate() {
            if !seen.contains(&h.queue) {
                seen.push(h.queue);
                out.push(i);
            }
        }
        out
    }

    /// True when held work of a queue other than `queue` fences `sync`.
    pub(crate) fn fenced_by_held(&self, sync: u64, queue: u64) -> bool {
        self.held.iter().any(|h| {
            h.queue != queue
                && h.marks
                    .as_ref()
                    .is_some_and(|m| m.contains(&MarkOp::FenceSync { sync }))
        })
    }

    /// The counter of an event: what the GPU signals left, else guest memory. None when the event has no counter.
    pub(crate) fn counter(&self, event: u64, mem: &impl Words) -> Option<(u64, u32)> {
        let word = mem.u64(event + EVENT_COUNTER).filter(|w| *w != 0)?;
        let v = self.events.get(&word).copied().or_else(|| mem.u32(word))?;
        Some((word, v))
    }

    /// The first wait of `marks`, the work of `queue`, that is not satisfied. The signals before it in `marks` count.
    pub(crate) fn blocker(&self, queue: u64, marks: &[MarkOp], mem: &impl Words) -> Option<MarkOp> {
        let mut local: HashMap<u64, u32> = HashMap::new();
        for m in marks {
            match *m {
                MarkOp::SignalEvent { event, mode, value } => {
                    if let Some((word, v)) = self.counter(event, mem) {
                        let old = local.get(&word).copied().unwrap_or(v);
                        local.insert(word, event_signalled(mode, old, value));
                    }
                }
                MarkOp::WaitEvent { event, mode, value } => {
                    let Some((word, v)) = self.counter(event, mem) else {
                        continue;
                    };
                    let current = local.get(&word).copied().unwrap_or(v);
                    if !event_wait_done(mode, current, value) {
                        return Some(*m);
                    }
                }
                MarkOp::WaitSync { sync } if self.fenced_by_held(sync, queue) => return Some(*m),
                _ => {}
            }
        }
        None
    }

    /// The work with `marks` ran: its signals change the event counters.
    pub(crate) fn ran(&mut self, marks: &[MarkOp], mem: &impl Words) {
        for m in marks {
            match *m {
                MarkOp::SignalEvent { event, mode, value } => {
                    self.stats.signals += 1;
                    if let Some((word, v)) = self.counter(event, mem) {
                        self.events.insert(word, event_signalled(mode, v, value));
                    }
                }
                MarkOp::FenceSync { .. } => self.stats.fences += 1,
                MarkOp::WaitEvent { .. } | MarkOp::WaitSync { .. } => self.stats.waits += 1,
                _ => {}
            }
        }
        for h in &mut self.held {
            h.passed += 1;
        }
    }

    /// The CPU wrote the counter at `word` (`nvnEventSignal`): guest memory has it now.
    pub(crate) fn cpu_signalled(&mut self, word: u64) {
        self.stats.cpu_signals += 1;
        self.events.remove(&word);
    }

    /// True when held work of a queue other than `queue` signals the event whose counter is at `word`. Only held ops
    /// whose marks were read count.
    pub(crate) fn signalled_by_held(&self, word: u64, queue: u64, mem: &impl Words) -> bool {
        self.held.iter().any(|h| {
            h.queue != queue
                && h.marks.as_ref().is_some_and(|m| {
                    m.iter().any(|op| match *op {
                        MarkOp::SignalEvent { event, .. } => {
                            self.counter(event, mem).is_some_and(|(w, _)| w == word)
                        }
                        _ => false,
                    })
                })
        })
    }

    /// The held op that a flush runs next, when nothing can run: the oldest head of a queue whose blocking wait no held
    /// work can satisfy, else the oldest. `blockers` has the blocking wait of each head (None when it may run).
    pub(crate) fn force_pick(
        &self,
        blockers: &[(usize, Option<MarkOp>)],
        mem: &impl Words,
    ) -> usize {
        let free = blockers.iter().find(|(i, b)| {
            let queue = self.held[*i].queue;
            match *b {
                None => true,
                Some(MarkOp::WaitEvent { event, .. }) => self
                    .counter(event, mem)
                    .is_none_or(|(word, _)| !self.signalled_by_held(word, queue, mem)),
                Some(MarkOp::WaitSync { sync }) => !self.fenced_by_held(sync, queue),
                Some(_) => true,
            }
        });
        free.or(blockers.first()).map_or(0, |(i, _)| *i)
    }

    /// The queues of held ops (not `queue`) that signal the event whose counter is at `word`, for the trace.
    pub(crate) fn held_signallers(&self, word: u64, queue: u64, mem: &impl Words) -> Vec<u64> {
        self.held
            .iter()
            .filter(|h| h.queue != queue)
            .filter(|h| {
                h.marks.as_ref().is_some_and(|m| {
                    m.iter().any(|op| match *op {
                        MarkOp::SignalEvent { event, .. } => {
                            self.counter(event, mem).is_some_and(|(w, _)| w == word)
                        }
                        _ => false,
                    })
                })
            })
            .map(|h| h.queue)
            .collect()
    }

    /// The queues of held ops (not `queue`) that fence `sync`, for the trace.
    pub(crate) fn held_fencers(&self, sync: u64, queue: u64) -> Vec<u64> {
        self.held
            .iter()
            .filter(|h| {
                h.queue != queue
                    && h.marks
                        .as_ref()
                        .is_some_and(|m| m.contains(&MarkOp::FenceSync { sync }))
            })
            .map(|h| h.queue)
            .collect()
    }

    /// The counter of an event as the gate sees it, for the diagnostic.
    pub(crate) fn event_value(&self, event: u64, mem: &impl Words) -> Option<u32> {
        self.counter(event, mem).map(|(_, v)| v)
    }
}

#[cfg(test)]
mod tests;
