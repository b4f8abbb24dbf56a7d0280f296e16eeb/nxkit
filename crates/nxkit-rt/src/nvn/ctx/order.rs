//! The submits of the queues through the gate ([`gate`](super::gate)): what runs now, what is held, and the
//! `NXKIT_DIAG=queues` report.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{MutexGuard, PoisonError};

use nxkit_nvn::backend::Fence;
use nxkit_nvn::cmdbuf::CommandHandle;
use nxkit_nvn::marks::MarkOp;

use super::gate::{Gate, Held, Op, Words};
use super::{Front, Gpu};
use crate::nvn::abi::{AbiResult, Obj, log_once};
use crate::nvn::gateseq::{self, note};
use crate::nvn::sync;

/// Guest words for the gate.
struct Guest;

impl Words for Guest {
    fn u32(&self, addr: u64) -> Option<u32> {
        let mut b = [0u8; 4];
        nxkit_abi::guest::read(addr, &mut b).ok()?;
        Some(u32::from_le_bytes(b))
    }

    fn u64(&self, addr: u64) -> Option<u64> {
        let mut b = [0u8; 8];
        nxkit_abi::guest::read(addr, &mut b).ok()?;
        Some(u64::from_le_bytes(b))
    }
}

/// Whether `NXKIT_DIAG=queues` is on.
fn diag() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| nxkit_nvn::diag::on("queues"))
}

/// Whether waits hold work: on unless `NXKIT_QUEUE_ORDER=0`.
pub(super) fn ordering_on() -> bool {
    std::env::var("NXKIT_QUEUE_ORDER").map_or(true, |v| v != "0")
}

fn op_name(op: &Op) -> &'static str {
    match op {
        Op::Submit { .. } => "submit",
        Op::Fence { .. } => "fence",
        Op::Wait { .. } => "wait",
    }
}

/// A mark that orders queues, for the trace of `NXKIT_DIAG=gateseq`.
fn describe(gate: &Gate, m: &MarkOp) -> Option<String> {
    let word = |event| {
        gate.counter(event, &Guest)
            .map_or("no counter".to_string(), |(w, v)| {
                format!("word {w:#x} counter {v}")
            })
    };
    Some(match *m {
        MarkOp::SignalEvent { event, mode, value } => {
            format!("signal ev {event:#x} ({}) mode {mode} val {value}", word(event))
        }
        MarkOp::WaitEvent { event, mode, value } => {
            format!("wait ev {event:#x} ({}) mode {mode} val {value}", word(event))
        }
        MarkOp::FenceSync { sync } => format!("fence sync {sync:#x}"),
        MarkOp::WaitSync { sync } => format!("waitsync {sync:#x}"),
        _ => return None,
    })
}

fn describe_marks(gate: &Gate, marks: &[MarkOp]) -> String {
    let v: Vec<String> = marks.iter().filter_map(|m| describe(gate, m)).collect();
    if v.is_empty() {
        "no waits or signals".to_string()
    } else {
        v.join("; ")
    }
}

/// What a blocked op of `queue` waits for and who would satisfy it.
fn why_blocked(gate: &Gate, queue: u64, b: &MarkOp) -> String {
    match *b {
        MarkOp::WaitEvent { event, mode, value } => {
            let Some((word, cur)) = gate.counter(event, &Guest) else {
                return format!("wait ev {event:#x} without counter");
            };
            format!(
                "wait ev {event:#x} word {word:#x} mode {mode} val {value}, counter {cur}; \
                 held signallers {:x?}; {}",
                gate.held_signallers(word, queue, &Guest),
                gateseq::last_event_signals(word)
            )
        }
        MarkOp::WaitSync { sync } => format!(
            "waitsync {sync:#x}; held fencers {:x?}; {}",
            gate.held_fencers(sync, queue),
            gateseq::last_fence(sync)
        ),
        _ => format!("{b:x?}"),
    }
}

impl Gpu {
    pub(super) fn gate(&self) -> MutexGuard<'_, Gate> {
        self.gate.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `nvnQueueSubmitCommands`. A game that never recorded a wait or an event signal goes straight to the frame.
    pub(crate) fn submit(
        &self,
        queue: u64,
        handles: &[CommandHandle],
        origin_lower_left: bool,
    ) -> AbiResult<()> {
        let mut front = self.lock();
        let mut gate = self.gate();
        let check = crate::nvn::cmdbuf::waits_recorded() || !gate.is_empty();
        if gateseq::active() {
            let mut m = Vec::new();
            let _ = front.queue_marks(queue as usize, handles, &mut m);
            note(format_args!(
                "SUBMIT q {queue:#x} recs {:x?} gated {check}, held ops {} | {}",
                handles.iter().map(|h| h.0).collect::<Vec<_>>(),
                gate.held.len(),
                describe_marks(&gate, &m)
            ));
        }
        if !check {
            drop(gate);
            let r = self.run_submit(&mut front, queue, handles, origin_lower_left);
            return r;
        }
        gate.push(
            queue,
            Op::Submit {
                handles: handles.to_vec(),
                origin: origin_lower_left,
            },
        );
        let r = self.pump(&mut front, &mut gate);
        drop(gate);
        r
    }

    /// `nvnQueueFenceSync`. The fence is the open frame when work is in it or held; a held queue keeps the fence
    /// behind its held work, so a wait of another queue on it waits for that work.
    pub(crate) fn queue_fence(&self, queue: u64, s: u64) -> AbiResult<()> {
        sync::set_queue_fence(Obj(s), self.fence_now())?;
        let _front = self.lock();
        let mut gate = self.gate();
        note(format_args!(
            "QUEUE FENCE q {queue:#x} sync {s:#x}, queue holds work {}",
            gate.holds(queue)
        ));
        if gate.holds(queue) {
            gate.push(queue, Op::Fence { sync: s });
        }
        Ok(())
    }

    /// `nvnQueueWaitSync`: the queue waits when held work of another queue fences the sync.
    pub(crate) fn queue_wait(&self, queue: u64, s: u64) -> AbiResult<()> {
        let mut front = self.lock();
        let mut gate = self.gate();
        note(format_args!(
            "QUEUE WAIT q {queue:#x} sync {s:#x}, queue holds work {}, fenced by held {} | {}",
            gate.holds(queue),
            gate.fenced_by_held(s, queue),
            gateseq::last_fence(s)
        ));
        if gate.holds(queue) || gate.fenced_by_held(s, queue) {
            gate.push(queue, Op::Wait { sync: s });
            let r = self.pump(&mut front, &mut gate);
            drop(gate);
            return r;
        }
        Ok(())
    }

    /// `nvnEventSignal` wrote the counter at `word`: a held wait for it may run now.
    pub(crate) fn event_signalled(&self, word: u64) -> AbiResult<()> {
        if gateseq::active() {
            note(format_args!(
                "CPU EVENT SIGNAL word {word:#x} counter now {:?}, held ops {}",
                Guest.u32(word),
                self.gate().held.len()
            ));
        }
        gateseq::cpu_signal(word);
        let held = {
            let mut gate = self.gate();
            gate.cpu_signalled(word);
            !gate.is_empty()
        };
        if held {
            let mut front = self.lock();
            let mut gate = self.gate();
            // The signal may release held submits into the frame: they are work now.
            let r = self.pump(&mut front, &mut gate);
            drop(gate);
            return r;
        }
        Ok(())
    }

    /// Runs every held op now (`nvnQueueFinalize`).
    pub(crate) fn release_held(&self) -> AbiResult<()> {
        note(format_args!("QUEUE FINALIZE releases the held ops"));
        let mut front = self.lock();
        let mut gate = self.gate();
        self.release_all(&mut front, &mut gate)
    }

    /// Runs every held op (a flush, a present, a finalised queue): the one whose wait no held work satisfies first
    /// ([`Gate::force_pick`]), then what it releases, until nothing is held.
    pub(super) fn release_all(&self, front: &mut Front, gate: &mut Gate) -> AbiResult<()> {
        let mut first = Ok(());
        while !gate.held.is_empty() {
            for j in 0..gate.held.len() {
                self.marks_of(front, &mut gate.held[j]);
            }
            let blockers: Vec<(usize, Option<MarkOp>)> = gate
                .heads()
                .into_iter()
                .map(|i| {
                    let h = &gate.held[i];
                    let b = h
                        .marks
                        .as_ref()
                        .and_then(|m| gate.blocker(h.queue, m, &Guest));
                    (i, b)
                })
                .collect();
            let i = gate.force_pick(&blockers, &Guest);
            let blocker = blockers.iter().find(|(j, _)| *j == i).and_then(|(_, b)| *b);
            let Some(h) = gate.held.remove(i) else {
                break;
            };
            gate.stats.forced += 1;
            if gateseq::active() {
                note(format_args!(
                    "FORCE q {:#x} {} at the flush, {} ops of other queues ran meanwhile | {}",
                    h.queue,
                    op_name(&h.op),
                    h.passed,
                    blocker.map_or("not blocked".to_string(), |b| why_blocked(gate, h.queue, &b))
                ));
            }
            if diag() {
                static SHOWN: AtomicU32 = AtomicU32::new(0);
                if nxkit_nvn::diag::first(&SHOWN, 100) {
                    let cur = match blocker {
                        Some(MarkOp::WaitEvent { event, .. }) => gate.event_value(event, &Guest),
                        _ => None,
                    };
                    eprintln!(
                        "DIAG queues forced q {:#x} {} at the flush on {blocker:x?} (counter now {cur:x?}), {} ops of other queues after it",
                        h.queue,
                        op_name(&h.op),
                        h.passed
                    );
                }
            }
            first = first.and(self.run_held(front, gate, h));
            if gate.on {
                first = first.and(self.pump(front, gate));
            }
        }
        first
    }

    /// Runs the oldest op of each queue whose waits are satisfied, again and again, oldest first.
    fn pump(&self, front: &mut Front, gate: &mut Gate) -> AbiResult<()> {
        let mut first = Ok(());
        'scan: loop {
            for i in gate.heads() {
                let marks = self.marks_of(front, &mut gate.held[i]);
                let queue = gate.held[i].queue;
                if let Some(b) = gate.blocker(queue, &marks, &Guest) {
                    if !gate.held[i].counted {
                        gate.held[i].counted = true;
                        if gateseq::active() {
                            note(format_args!(
                                "HOLD q {queue:#x} {} | {}",
                                op_name(&gate.held[i].op),
                                why_blocked(gate, queue, &b)
                            ));
                        }
                        let cur = match b {
                            MarkOp::WaitEvent { event, .. } => gate.event_value(event, &Guest),
                            _ => None,
                        };
                        if gate.on {
                            gate.stats.held += 1;
                        } else {
                            gate.stats.would_hold += 1;
                        }
                        if diag() {
                            static SHOWN: AtomicU32 = AtomicU32::new(0);
                            if nxkit_nvn::diag::first(&SHOWN, 100) {
                                eprintln!(
                                    "DIAG queues {} q {queue:#x} {} on {b:x?} (counter now {cur:x?})",
                                    if gate.on { "hold" } else { "would hold" },
                                    op_name(&gate.held[i].op)
                                );
                            }
                        }
                    }
                    if gate.on {
                        continue;
                    }
                }
                let Some(h) = gate.held.remove(i) else {
                    break 'scan;
                };
                if h.counted && gate.on {
                    gate.stats.released += 1;
                    note(format_args!(
                        "RELEASE q {:#x} {} after {} ops of other queues",
                        h.queue,
                        op_name(&h.op),
                        h.passed
                    ));
                    if diag() {
                        static SHOWN: AtomicU32 = AtomicU32::new(0);
                        if nxkit_nvn::diag::first(&SHOWN, 100) {
                            eprintln!(
                                "DIAG queues release q {:#x} {} after {} ops of other queues",
                                h.queue,
                                op_name(&h.op),
                                h.passed
                            );
                        }
                    }
                }
                let r = self.run_held(front, gate, h);
                first = first.and(r);
                continue 'scan;
            }
            break;
        }
        first
    }

    /// The waits and signals of a held op, read once.
    fn marks_of(&self, front: &mut Front, h: &mut Held) -> Vec<MarkOp> {
        if let Some(m) = &h.marks {
            return m.clone();
        }
        let mut m = Vec::new();
        if let Op::Submit { handles, .. } = &h.op
            && let Err(e) = front.queue_marks(h.queue as usize, handles, &mut m)
        {
            log_once(
                "nvnQueueSubmitCommands-marks",
                format_args!("nvnQueueSubmitCommands: the waits of a submit cannot be read: {e}"),
            );
        }
        h.marks = Some(m.clone());
        m
    }

    /// Runs a held op: a submit goes to the frame, and its fences and signals count from now.
    fn run_held(&self, front: &mut Front, gate: &mut Gate, mut h: Held) -> AbiResult<()> {
        let marks = self.marks_of(front, &mut h);
        if matches!(h.op, Op::Submit { .. }) {
            gate.stats.checked += 1;
        }
        if gateseq::active() {
            note(format_args!(
                "RUN q {:#x} {} into the frame ({} cmds so far) | {}",
                h.queue,
                op_name(&h.op),
                front.frame().cmds.len(),
                describe_marks(gate, &marks)
            ));
        }
        for m in &marks {
            match *m {
                MarkOp::SignalEvent { event, .. } => {
                    if let Some((word, _)) = gate.counter(event, &Guest) {
                        gateseq::gpu_signal(word, h.queue);
                    }
                }
                MarkOp::FenceSync { sync } => gateseq::fenced(sync, h.queue),
                _ => {}
            }
        }
        let r = match &h.op {
            Op::Submit { handles, origin } => self.run_submit(front, h.queue, handles, *origin),
            Op::Fence { .. } | Op::Wait { .. } => Ok(()),
        };
        // A fence in a recording marks its sync when the recording is in the frame, so `nvnSyncWait` waits for it.
        let fence = Fence(self.flushed() + 1);
        for m in &marks {
            if let (MarkOp::FenceSync { sync: s }, Op::Submit { .. }) = (m, &h.op) {
                let _ = sync::set_queue_fence(Obj(*s), fence);
            }
        }
        gate.ran(&marks, &Guest);
        r
    }

    /// `NXKIT_DIAG=queues`: one line for a flush with waits, holds or signals (the first 300, then every 60th).
    pub(super) fn gate_report(&self, gate: &mut Gate, fence: u64) {
        if !diag() {
            return;
        }
        let s = std::mem::take(&mut gate.stats);
        if s.is_empty() {
            return;
        }
        static SHOWN: AtomicU32 = AtomicU32::new(0);
        let n = SHOWN.fetch_add(1, Ordering::Relaxed);
        if n < 300 || n.is_multiple_of(60) {
            eprintln!(
                "DIAG queues flush {fence}: submits read {} | waits {} held {} would hold {} released by a signal {} forced at a flush {} | GPU signals {} fences {} CPU signals {}",
                s.checked,
                s.waits,
                s.held,
                s.would_hold,
                s.released,
                s.forced,
                s.signals,
                s.fences,
                s.cpu_signals
            );
        }
    }
}
