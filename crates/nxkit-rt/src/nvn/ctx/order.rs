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
        if !check {
            drop(gate);
            return self.run_submit(&mut front, queue, handles, origin_lower_left);
        }
        gate.push(
            queue,
            Op::Submit {
                handles: handles.to_vec(),
                origin: origin_lower_left,
            },
        );
        self.pump(&mut front, &mut gate)
    }

    /// `nvnQueueFenceSync`. The fence is the open frame when work is in it or held; a held queue keeps the fence
    /// behind its held work, so a wait of another queue on it waits for that work.
    pub(crate) fn queue_fence(&self, queue: u64, s: u64) -> AbiResult<()> {
        sync::set_queue_fence(Obj(s), self.fence_now())?;
        let _front = self.lock();
        let mut gate = self.gate();
        if gate.holds(queue) {
            gate.push(queue, Op::Fence { sync: s });
        }
        Ok(())
    }

    /// `nvnQueueWaitSync`: the queue waits when held work of another queue fences the sync.
    pub(crate) fn queue_wait(&self, queue: u64, s: u64) -> AbiResult<()> {
        let mut front = self.lock();
        let mut gate = self.gate();
        if gate.holds(queue) || gate.fenced_by_held(s, queue) {
            gate.push(queue, Op::Wait { sync: s });
            return self.pump(&mut front, &mut gate);
        }
        Ok(())
    }

    /// `nvnEventSignal` wrote the counter at `word`: a held wait for it may run now.
    pub(crate) fn event_signalled(&self, word: u64) -> AbiResult<()> {
        let held = {
            let mut gate = self.gate();
            gate.cpu_signalled(word);
            !gate.is_empty()
        };
        if held {
            let mut front = self.lock();
            let mut gate = self.gate();
            return self.pump(&mut front, &mut gate);
        }
        Ok(())
    }

    /// Runs every held op now (`nvnQueueFinalize`).
    pub(crate) fn release_held(&self) -> AbiResult<()> {
        let mut front = self.lock();
        let mut gate = self.gate();
        self.release_all(&mut front, &mut gate)
    }

    /// Runs every held op in the order it came (a flush, a present, a finalised queue).
    pub(super) fn release_all(&self, front: &mut Front, gate: &mut Gate) -> AbiResult<()> {
        let mut first = Ok(());
        while let Some(h) = gate.held.pop_front() {
            gate.stats.forced += 1;
            if diag() {
                static SHOWN: AtomicU32 = AtomicU32::new(0);
                if nxkit_nvn::diag::first(&SHOWN, 100) {
                    eprintln!(
                        "DIAG queues forced q {:#x} {} at the flush, {} ops of other queues after it",
                        h.queue,
                        op_name(&h.op),
                        h.passed
                    );
                }
            }
            let r = self.run_held(front, gate, h);
            first = first.and(r);
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
