//! GPU counters: `ResetCounter` and `ReportCounter` of the counters that the GPU counts, such as samples passed.
//!
//! NVN has no query objects. A game resets a counter of a queue (`nvnCommandBufferResetCounter`), draws, and writes
//! the counter into a 16-byte `NVNcounterData` (`nvnCommandBufferReportCounter`): the 64-bit counter, then the 64-bit
//! GPU time. An occlusion test reports samples passed before and after its boxes, at `addr` and `addr + 16`, and a
//! render condition (`crate::rendercond`) compares the two.
//!
//! The front end cuts the commands of each queue that uses a counter into slots: a slot ends at every reset and report
//! of the queue. [`FrameCounters`] holds the command ranges of the slots and, for every report, the slots that it
//! sums: the slots of its queue since the last reset of its counter in the frame (or since the start of the frame).
//! The backend counts each slot when it encodes the frame and writes the reports when the GPU finished it:
//!
//! - samples passed ([`SAMPLES_PASSED`]): the draws of a slot count samples into a visibility result of their own;
//! - vertices and primitives ([`INPUT_VERTICES`], [`INPUT_PRIMITIVES`], ...): the host adds them up from the
//!   arguments of the draws; an indirect draw makes the slot unknown;
//! - the other counters (the shader invocations of other stages, transform feedback, zcull) stay unknown.
//!
//! A report whose slots were all counted lands: the backend writes it to guest memory and into [`Landed`] with the
//! sequence number of the report, and the render conditions of later submits read it there. `NXKIT_COUNTERS=0`
//! turns all of this off: every counter that the GPU counts is unknown again.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use crate::frame::{Cmd, Frame};

/// `NVN_COUNTER_TYPE_TIMESTAMP`.
pub const TIMESTAMP: u32 = 0;
/// `NVN_COUNTER_TYPE_SAMPLES_PASSED`.
pub const SAMPLES_PASSED: u32 = 1;
/// `NVN_COUNTER_TYPE_INPUT_VERTICES`.
pub const INPUT_VERTICES: u32 = 2;
/// `NVN_COUNTER_TYPE_INPUT_PRIMITIVES`.
pub const INPUT_PRIMITIVES: u32 = 3;
/// `NVN_COUNTER_TYPE_VERTEX_SHADER_INVOCATIONS`.
pub const VERTEX_SHADER_INVOCATIONS: u32 = 4;
/// `NVN_COUNTER_TYPE_CLIPPER_INPUT_PRIMITIVES`.
pub const CLIPPER_INPUT_PRIMITIVES: u32 = 0xb;
/// `NVN_COUNTER_TYPE_PRIMITIVES_GENERATED`.
pub const PRIMITIVES_GENERATED: u32 = 0xd;
/// `NVN_COUNTER_TYPE_TIMESTAMP_TOP`.
pub const TIMESTAMP_TOP: u32 = 0x10;

/// The most entries of [`Landed`]. A full table starts again.
const MAX_LANDED: usize = 4096;

/// How the host knows a counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// A timestamp: the counter word is 0 and the backend writes the time.
    Timestamp,
    /// Samples that passed the depth and stencil tests: Metal visibility results.
    Samples,
    /// The vertices of the draws, from their arguments.
    Vertices,
    /// The primitives of the draws, from their arguments and topology.
    Primitives,
    /// A counter the host does not emulate.
    Unknown,
}

/// How the host knows the counter of `kind`. Unknown for every GPU counter with `NXKIT_COUNTERS=0`.
pub fn source(kind: u32) -> Source {
    match kind {
        TIMESTAMP | TIMESTAMP_TOP => Source::Timestamp,
        _ if !enabled() => Source::Unknown,
        SAMPLES_PASSED => Source::Samples,
        INPUT_VERTICES | VERTEX_SHADER_INVOCATIONS => Source::Vertices,
        INPUT_PRIMITIVES | CLIPPER_INPUT_PRIMITIVES | PRIMITIVES_GENERATED => Source::Primitives,
        _ => Source::Unknown,
    }
}

/// Whether the host emulates the GPU counters (`NXKIT_COUNTERS` is not `0`).
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("NXKIT_COUNTERS").map_or(true, |v| v != "0"))
}

/// A report that the backend writes when the GPU finished the frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CounterWrite {
    /// The GPU address of the 16-byte `NVNcounterData`.
    pub addr: u64,
    /// The `NVNcounterType`.
    pub kind: u32,
    /// The sequence number of the report, for [`Landed`].
    pub seq: u64,
    /// The slots that the counter sums, as a range of [`FrameCounters::sums`].
    pub sums: Range<u32>,
}

/// The counted slots and the reports of a frame.
#[derive(Clone, Debug, Default)]
pub struct FrameCounters {
    /// The commands of each slot, in command order and apart.
    pub ranges: Vec<(Range<u32>, u32)>,
    /// The number of slots.
    pub slots: u32,
    /// The slots of the reports (see [`CounterWrite::sums`]).
    pub sums: Vec<u32>,
    /// The reports, in command order.
    pub writes: Vec<CounterWrite>,
    /// Where the backend puts the counters that landed. Set when the frame has a report.
    pub landed: Option<Arc<Landed>>,
}

impl FrameCounters {
    /// Forgets the slots and the reports.
    pub fn clear(&mut self) {
        self.ranges.clear();
        self.slots = 0;
        self.sums.clear();
        self.writes.clear();
        self.landed = None;
    }

    /// The slot of command `i`, None when no counter counts it.
    pub fn slot_of(&self, i: u32) -> Option<u32> {
        let at = self.ranges.partition_point(|(r, _)| r.end <= i);
        self.ranges
            .get(at)
            .filter(|(r, _)| r.start <= i)
            .map(|(_, s)| *s)
    }

    /// The slots that a report sums.
    pub fn sums_of(&self, w: &CounterWrite) -> &[u32] {
        self.sums
            .get(w.sums.start as usize..w.sums.end as usize)
            .unwrap_or(&[])
    }

    fn range(&mut self, r: Range<u32>, slot: u32) {
        if r.is_empty() {
            return;
        }
        match self.ranges.last_mut() {
            Some((last, s)) if *s == slot && last.end == r.start => last.end = r.end,
            _ => self.ranges.push((r, slot)),
        }
    }
}

/// The counters that landed: the value and the sequence number of the last report written at each address.
#[derive(Debug, Default)]
pub struct Landed {
    map: Mutex<HashMap<u64, (u64, u64)>>,
}

impl Landed {
    /// The report `seq` wrote `value` at `addr`.
    pub fn land(&self, addr: u64, value: u64, seq: u64) {
        let mut m = self.map.lock().unwrap_or_else(PoisonError::into_inner);
        if m.len() >= MAX_LANDED && !m.contains_key(&addr) {
            m.clear();
        }
        let e = m.entry(addr).or_insert((value, seq));
        if seq >= e.1 {
            *e = (value, seq);
        }
    }

    /// The value and sequence number of the last report that landed at `addr`.
    pub fn get(&self, addr: u64) -> Option<(u64, u64)> {
        self.map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&addr)
            .copied()
    }
}

/// The slots of one queue in the open frame.
#[derive(Debug, Default)]
struct QueueSlots {
    /// The slots in order.
    slots: Vec<u32>,
    /// The slot that counts now. None after a reset or a report: the next command opens one.
    open: Option<u32>,
    /// The first command of the open range.
    from: u32,
    /// For each counter type, the index in `slots` of the first slot after its last reset.
    resets: HashMap<u32, usize>,
}

/// The counted slots of the queues that use a GPU counter.
#[derive(Debug, Default)]
pub struct Counters {
    queues: HashMap<usize, QueueSlots>,
    seq: u64,
    landed: Arc<Landed>,
}

impl Counters {
    /// No queue counts.
    pub fn new() -> Self {
        Self::default()
    }

    /// The counters that landed.
    pub fn landed(&self) -> &Arc<Landed> {
        &self.landed
    }

    /// A stream of `queue` starts at command `base` of the frame.
    pub fn begin(&mut self, queue: usize, base: u32) {
        if let Some(q) = self.queues.get_mut(&queue) {
            q.from = base;
        }
    }

    /// The stream of `queue` ends at command `end`: its commands since the last mark go to the open slot.
    pub fn end(&mut self, queue: usize, end: u32, frame: &mut Frame) {
        if let Some(q) = self.queues.get_mut(&queue) {
            close(q, end, frame);
        }
    }

    /// `ResetCounter` of `kind` before command `at`.
    pub fn reset(&mut self, queue: usize, base: u32, at: u32, kind: u32, frame: &mut Frame) {
        if !counted(kind) {
            return;
        }
        let q = self.queue(queue, base);
        close(q, at, frame);
        q.open = None;
        q.resets.insert(kind, q.slots.len());
    }

    /// `ReportCounter` of `kind` at `addr` before command `at`. The result is the sequence number of the report, None
    /// for a counter that the host does not count.
    pub fn report(
        &mut self,
        queue: usize,
        base: u32,
        at: u32,
        kind: u32,
        addr: u64,
        frame: &mut Frame,
    ) -> Option<u64> {
        if !counted(kind) {
            return None;
        }
        self.seq += 1;
        let seq = self.seq;
        let q = self.queue(queue, base);
        close(q, at, frame);
        q.open = None;
        let first = q.resets.get(&kind).copied().unwrap_or(0);
        let c = &mut frame.counters;
        let start = c.sums.len() as u32;
        c.sums.extend(q.slots.get(first..).unwrap_or(&[]));
        c.writes.push(CounterWrite {
            addr,
            kind,
            seq,
            sums: start..c.sums.len() as u32,
        });
        if c.landed.is_none() {
            c.landed = Some(self.landed.clone());
        }
        Some(seq)
    }

    /// The frame was flushed: every queue starts the next one without slots.
    pub fn flushed(&mut self) {
        for q in self.queues.values_mut() {
            q.slots.clear();
            q.open = None;
            q.resets.clear();
        }
    }

    /// Forgets a queue (`nvnQueueFinalize`).
    pub fn drop_queue(&mut self, queue: usize) {
        self.queues.remove(&queue);
    }

    fn queue(&mut self, queue: usize, base: u32) -> &mut QueueSlots {
        self.queues.entry(queue).or_insert_with(|| QueueSlots {
            from: base,
            ..QueueSlots::default()
        })
    }
}

/// True for a counter that the backend counts per slot.
fn counted(kind: u32) -> bool {
    !matches!(source(kind), Source::Timestamp | Source::Unknown)
}

/// The commands of `q` from its open range to `at` go to its open slot, which opens when they are not empty.
fn close(q: &mut QueueSlots, at: u32, frame: &mut Frame) {
    if at > q.from {
        let slot = *q.open.get_or_insert_with(|| {
            let s = frame.counters.slots;
            frame.counters.slots += 1;
            q.slots.push(s);
            s
        });
        frame.counters.range(q.from..at, slot);
    }
    q.from = q.from.max(at);
}

/// The draws of `frame` that a slot counts: every draw in its ranges that no false render condition leaves out.
pub fn counted_draws(frame: &Frame) -> impl Iterator<Item = (u32, u32)> + '_ {
    frame.counters.ranges.iter().flat_map(move |(r, slot)| {
        r.clone()
            .filter(move |i| {
                matches!(frame.cmds.get(*i as usize), Some(Cmd::Draw { .. })) && !frame.skips(*i)
            })
            .map(move |i| (i, *slot))
    })
}

/// The primitives of `vertices` vertices of the `NVNdrawPrimitive` `topology`, None for a topology the host does not
/// count (patches, adjacency).
pub fn primitives(topology: u32, vertices: u64) -> Option<u64> {
    let v = vertices;
    Some(match topology {
        0 | 2 => v,                   // points, line loop
        1 => v / 2,                   // lines
        3 => v.saturating_sub(1),     // line strip
        4 => v / 3,                   // triangles
        5 | 6 => v.saturating_sub(2), // triangle strip, fan
        7 => v / 4,                   // quads
        8 => v.saturating_sub(2) / 2, // quad strip
        9 => u64::from(v >= 3),       // polygon
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
