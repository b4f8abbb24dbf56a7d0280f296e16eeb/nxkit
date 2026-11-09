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
//! - The counters that the GPU counts (samples passed, vertices, primitives) are emulated (`crate::counters`): the
//!   backend writes them when the GPU finished the frame, and they land in [`Landed`]. A condition reads a counter
//!   once landed: the value of the report it names when that report landed, else the last value that landed at the
//!   address (a report of an earlier frame: one frame of latency, `NXKIT_RENDERCOND_STALE=0` turns that off), else
//!   it is unknown.
//! - The other counters are unknown. An unknown condition renders, as the host did before.
//!
//! The known values stay in the table for [`KEEP_FLUSHES`] flushes; by then the backend wrote them to guest memory.
//! The GPU counters stay until a known value replaces them.
//!
//! The diagnostic switches of [`force`] change a decision at run time. They are off by default. [`gate`] records what
//! each condition covered, when `NXKIT_DIAG=rendercond` is on.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::counters::{Counters, Landed, Place, Source};
use crate::frame::{Cmd, Frame};
use crate::marks::{Mark, MarkOp, RENDER_IF_EQUAL, RENDER_IF_NOT_EQUAL};
use crate::mem::GuestMem;
use crate::space::{GpuAddr, GpuSpace};

/// The flushes that a known report value stays in the table.
pub const KEEP_FLUSHES: u64 = 8;
/// The most entries of the report table. A full table drops its known values.
const MAX_REPORTS: usize = 1 << 16;
/// What the table knows of the counter of a report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Report {
    /// A counter that a recording wrote, with the flush of its frame.
    Known { value: u64, flush: u64 },
    /// A counter that the host emulates: report `seq` at `place` writes it when the GPU finished its frame.
    Gpu { seq: u64, place: Place },
    /// A counter that only the GPU knows.
    Unknown,
}

/// What a condition reads of one counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Read {
    /// A value of a recording (a report value or a timestamp).
    Known(u64),
    /// A word of guest memory that no report of the table names. It counts as known everywhere else.
    Memory(u64),
    /// The emulated counter of the report that the table names, landed.
    Landed(u64),
    /// The last emulated counter that landed at the address, of an earlier report.
    Stale(u64),
    /// The last emulated counter that landed at the place of the report in an earlier frame (another address).
    Place(u64),
    /// Nothing that the host knows.
    Unknown,
}

impl Read {
    /// The value, None when the host does not know it.
    pub fn value(self) -> Option<u64> {
        match self {
            Self::Known(v)
            | Self::Memory(v)
            | Self::Landed(v)
            | Self::Stale(v)
            | Self::Place(v) => Some(v),
            Self::Unknown => None,
        }
    }

    /// The word of the diagnostic lines for where the value came from.
    pub fn kind(self) -> &'static str {
        match self {
            Self::Known(_) => "known",
            Self::Memory(_) => "memory",
            Self::Landed(_) => "landed",
            Self::Stale(_) => "stale",
            Self::Place(_) => "place",
            Self::Unknown => "unknown",
        }
    }
}

/// `NXKIT_RENDERCOND_STALE`: `0` reads no counter of an earlier report, `addr` only the one at the same address; by
/// default the one at the same address, else the one at the same place of an earlier frame. The switch
/// [`force::NOSTALE`] gives 0.
fn stale_mode() -> u8 {
    if force::force() & force::NOSTALE != 0 {
        return 0;
    }
    static MODE: OnceLock<u8> = OnceLock::new();
    *MODE.get_or_init(
        || match std::env::var("NXKIT_RENDERCOND_STALE").as_deref() {
            Ok("0") => 0,
            Ok("addr") => 1,
            _ => 2,
        },
    )
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
    /// Conditions that read emulated counters of the reports they name, landed.
    pub landed: u32,
    /// Conditions that read the counter of an earlier report at an address (one frame of latency).
    pub stale: u32,
    /// Conditions that read the counter of the report at the same place of an earlier frame.
    pub placed: u32,
    /// Emulated reports that sum no draw: 0, known when they are recorded.
    pub zero: u32,
    /// `ReportValue` writes that the table took.
    pub values: u32,
    /// Counter reports that the host emulates (`crate::counters`).
    pub emulated: u32,
    /// Counter reports that only the GPU knows.
    pub counters: u32,
    /// The GPU counter reports by `NVNcounterType` (16 and above in the last), emulated or not.
    pub kinds: [u32; 17],
    /// Decisions that a diagnostic switch forced, by switch: `rcflag`, `rcunk`, `rcall`.
    pub forced: [u32; 3],
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
    /// The slots and reports of the GPU counters.
    counters: Counters,
    /// The one-shot lines of `NXKIT_DIAG=rendercond` per condition address.
    diag: probe::Probe,
    /// The condition that each queue carries into its next submit, for the gate lines.
    gates: gate::Gates,
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
        self.counters.begin(queue, base);
        let rec = gate::enabled();
        let mut cond = if rec { self.gates.get(queue) } else { None };
        let mut cover_from = base;
        if marks.is_empty() && on && cond.is_none() {
            self.counters.end(queue, frame.cmds.len() as u32, frame);
            return;
        }
        let mut from = base;
        for m in marks {
            let at = base + m.at;
            let (next, info) = match m.op {
                MarkOp::RenderEnable(v) => {
                    self.stats.enables += 1;
                    self.stats.disables += u32::from(!v);
                    (v, None)
                }
                MarkOp::RenderIf { mode, addr } => {
                    let i = self.evaluate(mode, addr, &read);
                    (i.decision.render, Some(i))
                }
                MarkOp::ReportValue { addr, value } => {
                    self.stats.values += 1;
                    let value = u64::from(value);
                    let flush = self.flush;
                    self.report(addr, Report::Known { value, flush });
                    continue;
                }
                MarkOp::ReportCounter { addr, kind } => {
                    self.counter_report(queue, base, at, kind, addr, frame);
                    continue;
                }
                MarkOp::ResetCounter { kind } => {
                    self.counters.reset(queue, base, at, kind, frame);
                    continue;
                }
                _ => continue,
            };
            if !on {
                frame.disable(from..at);
            }
            if rec {
                Self::cover(frame, cover_from..at, cond);
                (cond, cover_from) = (info, at);
            }
            (on, from) = (next, at);
        }
        if !on {
            frame.disable(from..frame.cmds.len() as u32);
        }
        if rec {
            Self::cover(frame, cover_from..frame.cmds.len() as u32, cond);
            self.gates.set(queue, cond);
        }
        self.off.insert(queue, !on);
        self.counters.end(queue, frame.cmds.len() as u32, frame);
    }

    /// `ReportCounter` of `kind` at `addr` before command `at`.
    fn counter_report(
        &mut self,
        queue: usize,
        base: u32,
        at: u32,
        kind: u32,
        addr: u64,
        frame: &mut Frame,
    ) {
        let source = crate::counters::source(kind);
        if source == Source::Timestamp {
            self.report(
                addr,
                Report::Known {
                    value: 0,
                    flush: self.flush,
                },
            );
            return;
        }
        self.stats.kinds[kind.min(16) as usize] += 1;
        let r = match self.counters.report(queue, base, at, kind, addr, frame) {
            Some(id) => {
                self.stats.emulated += 1;
                self.stats.zero += u32::from(id.zero);
                self.diag.reported(addr);
                if id.zero {
                    Report::Known {
                        value: 0,
                        flush: self.flush,
                    }
                } else {
                    Report::Gpu {
                        seq: id.seq,
                        place: id.place,
                    }
                }
            }
            None => {
                self.stats.counters += 1;
                Report::Unknown
            }
        };
        self.report(addr, r);
    }

    /// Decides the condition. An unknown counter or mode renders, unless a switch of [`force`] says otherwise.
    fn evaluate(&mut self, mode: u32, addr: u64, read: &impl Fn(u64) -> Option<u64>) -> gate::Info {
        self.evaluate_with(mode, addr, read, force::force())
    }

    /// [`RenderConds::evaluate`] with the switches `bits`, so that a test does not depend on the global ones.
    fn evaluate_with(
        &mut self,
        mode: u32,
        addr: u64,
        read: &impl Fn(u64) -> Option<u64>,
        bits: u8,
    ) -> gate::Info {
        let (ra, rb) = (
            self.counter(addr, read),
            self.counter(addr.wrapping_add(16), read),
        );
        let (a, b) = (ra.value(), rb.value());
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
        if r.is_some() {
            if matches!(ra, Read::Place(_)) || matches!(rb, Read::Place(_)) {
                self.stats.placed += 1;
            } else if matches!(ra, Read::Stale(_)) || matches!(rb, Read::Stale(_)) {
                self.stats.stale += 1;
            } else if matches!(ra, Read::Landed(_)) || matches!(rb, Read::Landed(_)) {
                self.stats.landed += 1;
            }
        }
        self.probe(mode, addr, ra, rb, r);
        if self.evaluations.len() < 64 {
            self.evaluations.push((mode, addr, a, b, r));
        }
        let source = force::source(r, ra, rb);
        let decision = force::decide(r, source, bits);
        if let Some(k) = [force::RCFLAG, force::RCUNK, force::RCALL]
            .iter()
            .position(|b| *b == decision.forced)
        {
            self.stats.forced[k] += 1;
        }
        gate::Info {
            mode,
            addr,
            reads: [ra, rb],
            source,
            decision,
        }
    }

    fn counter(&self, addr: u64, read: &impl Fn(u64) -> Option<u64>) -> Read {
        match self.reports.get(&addr) {
            Some(Report::Known { value, flush }) if self.flush - flush < KEEP_FLUSHES => {
                Read::Known(*value)
            }
            Some(Report::Gpu { seq, place }) => {
                let landed = self.counters.landed();
                match landed.get(addr) {
                    Some((v, s)) if s == *seq => Read::Landed(v),
                    Some((v, _)) if stale_mode() >= 1 => Read::Stale(v),
                    _ if stale_mode() >= 2 => landed
                        .at_place(*place)
                        .map_or(Read::Unknown, |(v, _)| Read::Place(v)),
                    _ => Read::Unknown,
                }
            }
            Some(Report::Unknown) => Read::Unknown,
            _ => read(addr).map_or(Read::Unknown, Read::Memory),
        }
    }

    fn report(&mut self, addr: u64, r: Report) {
        if self.reports.len() >= MAX_REPORTS {
            // The known values and the older half of the emulated counters go; the unknown ones stay. A table that is
            // still more than half full starts again, so the scan never runs for every report.
            let keep = self
                .counters
                .newest_seq()
                .saturating_sub(MAX_REPORTS as u64 / 2);
            self.reports.retain(|_, r| match r {
                Report::Known { .. } => false,
                Report::Gpu { seq, .. } => *seq >= keep,
                Report::Unknown => true,
            });
            if self.reports.len() >= MAX_REPORTS / 2 {
                self.reports.clear();
            }
        }
        self.reports.insert(addr, r);
    }

    /// The counters that landed, for the backend and the tests.
    pub fn landed(&self) -> &std::sync::Arc<Landed> {
        self.counters.landed()
    }

    /// The frame was flushed. Known values age by one flush.
    pub fn flushed(&mut self) {
        self.flush += 1;
        self.counters.flushed();
        self.diag.flushed();
    }

    /// Forgets the condition and the counter slots of a queue (`nvnQueueFinalize`).
    pub fn drop_queue(&mut self, queue: usize) {
        self.off.remove(&queue);
        self.counters.drop_queue(queue);
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
/// frames, then every 60th), and the first evaluations with the counters they read. The backend adds a line for the
/// emulated counters it wrote (`nxkit-gpu`, `queries`).
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
        let kinds: Vec<String> = s
            .kinds
            .iter()
            .enumerate()
            .filter(|(_, n)| **n > 0)
            .map(|(k, n)| format!("{k:#x}:{n}"))
            .collect();
        let forced = if s.forced == [0; 3] {
            String::new()
        } else {
            format!(
                " | forced decisions rcflag {} rcunk {} rcall {}",
                s.forced[0], s.forced[1], s.forced[2]
            )
        };
        eprintln!(
            "DIAG rendercond frame {n}: enable {} (off {}), conditions true {} false {} unknown {} (read landed {} stale {} by place {}), reports value {} emulated {} (zero {}) gpu-only {} by type [{}] in {} slots | left out: draws {d} clears {c} dispatches {x} copies {k} in {} ranges of {} commands{forced}",
            s.enables,
            s.disables,
            s.true_,
            s.false_,
            s.unknown,
            s.landed,
            s.stale,
            s.placed,
            s.values,
            s.emulated,
            s.zero,
            s.counters,
            kinds.join(" "),
            frame.counters.slots,
            frame.disabled.len(),
            frame.cmds.len()
        );
    }
}

pub mod force;
pub mod gate;
mod probe;
#[cfg(test)]
mod tests;
