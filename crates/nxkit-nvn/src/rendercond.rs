//! Render conditions: `SetRenderEnable` and `SetRenderEnableConditional`.
//!
//! A false render condition makes the GPU leave out the draws, clears, copies and dispatches after it until the next
//! condition. The front end follows the condition of each queue when it appends a stream to the frame, and marks the
//! commands that run under a false one in [`Frame::disabled`]. The analysis and the encoder leave them out.
//!
//! A conditional render reads the 64-bit counters of two 16-byte reports, at `addr` and `addr + 16`, when the GPU
//! reaches the command, and compares them. The host evaluates it when the stream is appended, in command order, which
//! is the order of the GPU on the queue. A report that an earlier command of a submitted stream writes has not reached
//! guest memory then (the backend writes reports when the GPU finished the frame), so the front end keeps the reports
//! that it saw in a table, and a condition reads that table before guest memory:
//!
//! - `ReportValue` writes a value that the recording names, so the table knows it.
//! - The timestamp counters (`TIMESTAMP`, `TIMESTAMP_TOP`) write a counter of 0.
//! - The other counters (samples passed, primitives) are counted by the GPU. The host does not know them, so a
//!   condition that reads one is unknown, and an unknown condition renders, as the host did before.
//!
//! The known values stay in the table for [`KEEP_FLUSHES`] flushes; by then the backend wrote them to guest memory.
//! The unknown ones stay until a known value replaces them. A CPU read back of the condition (GPU predication) is not
//! needed, because every value the host can know is known when the stream is appended.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::frame::{Cmd, Frame};
use crate::marks::{Mark, MarkOp, RENDER_IF_EQUAL, RENDER_IF_NOT_EQUAL};
use crate::mem::GuestMem;
use crate::space::{GpuAddr, GpuSpace};

/// The flushes that a known report value stays in the table.
pub const KEEP_FLUSHES: u64 = 8;
/// The most entries of the report table. A full table drops its known values.
const MAX_REPORTS: usize = 4096;
/// `NVN_COUNTER_TYPE_TIMESTAMP` and `NVN_COUNTER_TYPE_TIMESTAMP_TOP`: their counter word is 0.
const TIMESTAMPS: [u32; 2] = [0, 0x10];

/// What the table knows of the counter of a report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Report {
    /// A counter that a recording wrote, with the flush of its frame.
    Known { value: u64, flush: u64 },
    /// A counter that only the GPU knows.
    Unknown,
}

/// The counters of the render conditions since the last [`RenderConds::take_stats`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CondStats {
    /// `SetRenderEnable` calls.
    pub enables: u32,
    /// `SetRenderEnable(false)` calls.
    pub disables: u32,
    /// Conditions that were true.
    pub true_: u32,
    /// Conditions that were false.
    pub false_: u32,
    /// Conditions that read a counter only the GPU knows, or had an unknown mode. They render.
    pub unknown: u32,
    /// `ReportValue` writes that the table took.
    pub values: u32,
    /// Counter reports that only the GPU knows.
    pub counters: u32,
}

impl CondStats {
    /// True when no condition was seen.
    pub fn is_empty(&self) -> bool {
        self.enables + self.true_ + self.false_ + self.unknown == 0
    }
}

/// One evaluation, for the diagnostic: mode, address, the two counters and the result.
pub type Evaluation = (u32, u64, Option<u64>, Option<u64>, Option<bool>);

/// The render condition of each queue and the report table.
#[derive(Debug, Default)]
pub struct RenderConds {
    /// False while a queue renders nothing. A queue that is not here renders.
    off: HashMap<usize, bool>,
    reports: HashMap<u64, Report>,
    flush: u64,
    stats: CondStats,
    /// The first evaluations, for `NXKIT_DIAG=rendercond`.
    pub evaluations: Vec<Evaluation>,
}

impl RenderConds {
    /// An empty state: every queue renders.
    pub fn new() -> Self {
        Self::default()
    }

    /// Follows the marks of a stream that was appended to `frame` at command `base` on `queue`, and disables the
    /// commands that run under a false condition. `read` gives the 64-bit word of guest memory at a GPU address.
    pub fn apply(
        &mut self,
        queue: usize,
        base: u32,
        marks: &[Mark],
        frame: &mut Frame,
        read: impl Fn(u64) -> Option<u64>,
    ) {
        let mut on = !self.off.get(&queue).copied().unwrap_or(false);
        if marks.is_empty() && on {
            return;
        }
        let mut from = base;
        for m in marks {
            let at = base + m.at;
            let next = match m.op {
                MarkOp::RenderEnable(v) => {
                    self.stats.enables += 1;
                    self.stats.disables += u32::from(!v);
                    v
                }
                MarkOp::RenderIf { mode, addr } => self.evaluate(mode, addr, &read),
                MarkOp::ReportValue { addr, value } => {
                    self.stats.values += 1;
                    self.report(addr, Some(u64::from(value)));
                    continue;
                }
                MarkOp::ReportCounter { addr, kind } => {
                    self.stats.counters += u32::from(!TIMESTAMPS.contains(&kind));
                    self.report(addr, TIMESTAMPS.contains(&kind).then_some(0));
                    continue;
                }
                _ => continue,
            };
            if !on {
                frame.disable(from..at);
            }
            (on, from) = (next, at);
        }
        if !on {
            frame.disable(from..frame.cmds.len() as u32);
        }
        self.off.insert(queue, !on);
    }

    /// Whether the condition renders. An unknown counter or mode renders.
    fn evaluate(&mut self, mode: u32, addr: u64, read: &impl Fn(u64) -> Option<u64>) -> bool {
        let a = self.counter(addr, read);
        let b = self.counter(addr.wrapping_add(16), read);
        let r = match (mode, a, b) {
            (RENDER_IF_EQUAL, Some(a), Some(b)) => Some(a == b),
            (RENDER_IF_NOT_EQUAL, Some(a), Some(b)) => Some(a != b),
            _ => None,
        };
        match r {
            Some(true) => self.stats.true_ += 1,
            Some(false) => self.stats.false_ += 1,
            None => self.stats.unknown += 1,
        }
        if self.evaluations.len() < 64 {
            self.evaluations.push((mode, addr, a, b, r));
        }
        r.unwrap_or(true)
    }

    fn counter(&self, addr: u64, read: &impl Fn(u64) -> Option<u64>) -> Option<u64> {
        match self.reports.get(&addr) {
            Some(Report::Known { value, flush }) if self.flush - flush < KEEP_FLUSHES => {
                Some(*value)
            }
            Some(Report::Unknown) => None,
            _ => read(addr),
        }
    }

    fn report(&mut self, addr: u64, value: Option<u64>) {
        if self.reports.len() >= MAX_REPORTS {
            self.reports.retain(|_, r| *r == Report::Unknown);
        }
        let r = value.map_or(Report::Unknown, |value| Report::Known {
            value,
            flush: self.flush,
        });
        self.reports.insert(addr, r);
    }

    /// The frame was flushed. Known values age by one flush.
    pub fn flushed(&mut self) {
        self.flush += 1;
    }

    /// Forgets the condition of a queue (`nvnQueueFinalize`).
    pub fn drop_queue(&mut self, queue: usize) {
        self.off.remove(&queue);
    }

    /// The counters since the last call.
    pub fn take_stats(&mut self) -> CondStats {
        std::mem::take(&mut self.stats)
    }
}

/// The commands of `frame` that a false condition leaves out, by kind: draws, clears, dispatches, copies.
pub fn disabled_counts(frame: &Frame) -> [u32; 4] {
    let mut n = [0u32; 4];
    for r in &frame.disabled {
        for i in r.clone() {
            if !frame.skips(i) {
                continue;
            }
            let k = match frame.cmds.get(i as usize) {
                Some(Cmd::Draw { .. }) => 0,
                Some(Cmd::ClearColor { .. } | Cmd::ClearDepthStencil { .. }) => 1,
                Some(Cmd::Dispatch { .. }) => 2,
                _ => 3,
            };
            n[k] += 1;
        }
    }
    n
}

/// The little-endian word at a GPU address, None outside guest memory.
pub(crate) fn read_gpu_u64<S: GpuSpace, M: GuestMem>(space: &S, mem: &M, addr: u64) -> Option<u64> {
    let host = space.host_address(GpuAddr::new(addr).ok()?, 8).ok()?;
    let mut b = [0u8; 8];
    mem.read(host, &mut b).ok()?;
    Some(u64::from_le_bytes(b))
}

/// `NXKIT_DIAG=rendercond`: one line for a frame that set a condition or left something out (the first 300 such
/// frames, then every 60th), and the first evaluations with the counters they read.
pub(crate) fn frame_diag(rc: &mut RenderConds, frame: &Frame) {
    static ON: OnceLock<bool> = OnceLock::new();
    if !*ON.get_or_init(|| crate::diag::on("rendercond")) {
        return;
    }
    static FRAMES: AtomicU32 = AtomicU32::new(0);
    static LINES: AtomicU32 = AtomicU32::new(0);
    let s = rc.take_stats();
    let n = FRAMES.fetch_add(1, Ordering::Relaxed);
    for (mode, addr, a, b, r) in rc.evaluations.drain(..) {
        if crate::diag::first(&LINES, 200) {
            eprintln!(
                "DIAG rendercond frame {n}: if {} at {addr:#x}: counters {a:x?} and {b:x?} -> {}",
                if mode == RENDER_IF_EQUAL {
                    "equal"
                } else if mode == RENDER_IF_NOT_EQUAL {
                    "not equal"
                } else {
                    "unknown mode"
                },
                r.map_or("unknown, renders", |r| if r {
                    "renders"
                } else {
                    "left out"
                })
            );
        }
    }
    rc.evaluations.shrink_to(64);
    if s.is_empty() && frame.disabled.is_empty() {
        return;
    }
    static SHOWN: AtomicU32 = AtomicU32::new(0);
    let shown = SHOWN.fetch_add(1, Ordering::Relaxed);
    if shown < 300 || shown.is_multiple_of(60) {
        let [d, c, x, k] = disabled_counts(frame);
        eprintln!(
            "DIAG rendercond frame {n}: enable {} (off {}), conditions true {} false {} unknown {}, reports value {} gpu-only {} | left out: draws {d} clears {c} dispatches {x} copies {k} in {} ranges of {} commands",
            s.enables,
            s.disables,
            s.true_,
            s.false_,
            s.unknown,
            s.values,
            s.counters,
            frame.disabled.len(),
            frame.cmds.len()
        );
    }
}

#[cfg(test)]
mod tests;
