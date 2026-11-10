//! The GPU counters of a frame (`nxkit_nvn::counters`): the samples of the draws of each slot, counted by Metal, and
//! the vertices and primitives, added up from the draw arguments; written into the reports when the GPU finished.
//!
//! [`prepare`] runs before the encode. It gives every render pass with draws of a slot that counts samples offsets of
//! its own in a zeroed visibility result buffer (one per slot of the pass), so no two passes add to the same 8 bytes,
//! whichever command buffer runs them. The encoder sets the offset of each draw (`encode::render_pass`) and counts the
//! draws it encoded per slot. A slot whose draws were not all encoded (a pipeline still compiling, a lost pass) is not
//! counted: its reports do not land, and a render condition reads the counter of an earlier report instead.
//!
//! [`FrameQueries::write`] runs when the GPU finished the frame (`GpuBackend::write_counters`): every report whose
//! slots were all counted gets its 16-byte `NVNcounterData` (counter, then the GPU end time in ticks) in guest memory,
//! when the 16 bytes lie in it, and lands in [`nxkit_nvn::counters::Landed`].

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use nxkit_metal::{Context, DataBuffer, GuestSpace};
use nxkit_nvn::counters::{FrameCounters, Source, counted_draws, primitives, source};
use nxkit_nvn::frame::{Cmd, Frame};
use nxkit_nvn::passes::{PassKind, PassPlan};

use crate::detail::{DrawArgs, FrameDetail};
use crate::draws::{FastMap, Ordinal};

/// The visibility buffers that wait for a frame, at most this many.
const POOL: usize = 8;

/// Visibility result buffers to use again.
#[derive(Default)]
pub(crate) struct VisPool(Mutex<Vec<Arc<DataBuffer>>>);

impl VisPool {
    fn take(&self, ctx: &Context, len: u64) -> Option<Arc<DataBuffer>> {
        let reuse = {
            let mut p = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            p.iter()
                .position(|b| b.len() >= len)
                .map(|i| p.swap_remove(i))
        };
        match reuse {
            Some(b) => b.write(0, &vec![0u8; len as usize]).ok().map(|()| b),
            None => DataBuffer::new(ctx, len.next_power_of_two().max(256))
                .ok()
                .map(Arc::new),
        }
    }

    fn give(&self, b: Arc<DataBuffer>) {
        let mut p = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if p.len() < POOL {
            p.push(b);
        }
    }
}

/// The vertices and primitives of the draws of a slot. None when a draw has no arguments the host can read.
#[derive(Clone, Copy, Debug, Default)]
struct CpuCount {
    vertices: Option<u64>,
    primitives: Option<u64>,
}

/// The counters of one encoded frame, until the GPU finished it.
pub(crate) struct FrameQueries {
    counters: FrameCounters,
    pool: Arc<VisPool>,
    /// The visibility results, 8 bytes per offset. None when no report counts samples.
    buffer: Option<Arc<DataBuffer>>,
    /// The offsets of each render pass with counted draws, by pass index: (slot, offset index).
    pass_offsets: FastMap<usize, Vec<(u32, u32)>>,
    /// The slot of each offset index.
    offset_slot: Vec<u32>,
    /// The slot that counts the samples of each draw, by draw ordinal.
    draw_slot: Vec<Option<u32>>,
    /// The draws of each slot that count samples, and how many the encoder encoded.
    expected: Vec<u32>,
    encoded: Vec<AtomicU32>,
    cpu: Vec<CpuCount>,
    /// The mesh draws of `NXKIT_DIAG=meshvis`, with offsets after those of the reports.
    mesh: Option<crate::meshvis::Plan>,
}

impl std::fmt::Debug for FrameQueries {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameQueries")
            .field("slots", &self.counters.slots)
            .field("writes", &self.counters.writes.len())
            .field("offsets", &self.offset_slot.len())
            .finish()
    }
}

impl Drop for FrameQueries {
    /// The backend drops the counters of a frame after the GPU finished it, so its buffer is free again. (The command
    /// buffer keeps a reference too, which only keeps the memory alive.)
    fn drop(&mut self) {
        if let Some(b) = self.buffer.take() {
            self.pool.give(b);
        }
    }
}

/// The visibility offsets of one render pass.
pub(crate) struct PassVis<'a> {
    pub(crate) buffer: &'a DataBuffer,
    offsets: &'a [(u32, u32)],
}

impl PassVis<'_> {
    /// The byte offset of the samples of `slot` in this pass.
    pub(crate) fn offset(&self, slot: u32) -> Option<u64> {
        self.offsets
            .iter()
            .find(|(s, _)| *s == slot)
            .map(|(_, o)| u64::from(*o) * 8)
    }
}

/// What [`FrameQueries::write`] did, for `NXKIT_DIAG=rendercond`.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct WriteStats {
    pub(crate) written: u32,
    pub(crate) samples: u32,
    pub(crate) cpu: u32,
    /// Reports with a slot whose draws were not all encoded, or with an indirect draw.
    pub(crate) uncounted: u32,
    /// Reports whose 16 bytes are not in guest memory.
    pub(crate) refused: u32,
}

/// The counters of `frame`, None when it has no report.
pub(crate) fn prepare(
    ctx: &Context,
    pool: &Arc<VisPool>,
    frame: &Frame,
    plan: &PassPlan,
    detail: &FrameDetail,
    ords: &[Ordinal],
    mut mesh: Option<crate::meshvis::Plan>,
) -> Option<Arc<FrameQueries>> {
    let c = &frame.counters;
    let extra = mesh.as_ref().map_or(0, crate::meshvis::Plan::wanted);
    if c.writes.is_empty() && extra == 0 {
        return None;
    }
    let slots = c.slots as usize;
    let mut samples = vec![false; slots];
    for w in c
        .writes
        .iter()
        .filter(|w| source(w.kind) == Source::Samples)
    {
        for s in c.sums_of(w) {
            if let Some(v) = samples.get_mut(*s as usize) {
                *v = true;
            }
        }
    }
    let draws = ords.last().map_or(0, |o| o.draw as usize + 1);
    let mut draw_slot = vec![None; draws.max(detail.draws.len())];
    let mut expected = vec![0u32; slots];
    let mut cpu = vec![
        CpuCount {
            vertices: Some(0),
            primitives: Some(0),
        };
        slots
    ];
    for (i, slot) in counted_draws(frame) {
        let (s, ord) = (slot as usize, ords.get(i as usize).map(|o| o.draw as usize));
        if samples.get(s).copied().unwrap_or(false)
            && let Some(d) = ord.and_then(|o| draw_slot.get_mut(o))
        {
            *d = Some(slot);
            expected[s] += 1;
        }
        if let Some(n) = cpu.get_mut(s) {
            let draw = ord.and_then(|o| detail.draws.get(o));
            let v = draw.and_then(|d| vertices(&d.args));
            n.vertices = n.vertices.zip(v).map(|(a, b)| a + b);
            let p = draw.zip(v).and_then(|(d, v)| primitives(d.topology, v));
            n.primitives = n.primitives.zip(p).map(|(a, b)| a + b);
        }
    }
    let mut pass_offsets: FastMap<usize, Vec<(u32, u32)>> = FastMap::default();
    let mut offset_slot = Vec::new();
    for (pi, p) in plan.passes.iter().enumerate() {
        if p.kind != PassKind::Render {
            continue;
        }
        let mut mine: Vec<(u32, u32)> = Vec::new();
        for i in p.cmds.clone() {
            if !matches!(frame.cmds.get(i as usize), Some(Cmd::Draw { .. })) {
                continue;
            }
            let slot = ords
                .get(i as usize)
                .and_then(|o| draw_slot.get(o.draw as usize).copied().flatten());
            if let Some(slot) = slot
                && !mine.iter().any(|(s, _)| *s == slot)
            {
                mine.push((slot, offset_slot.len() as u32));
                offset_slot.push(slot);
            }
        }
        if !mine.is_empty() {
            pass_offsets.insert(pi, mine);
        }
    }
    let buffer = if offset_slot.is_empty() && extra == 0 {
        None
    } else {
        // Without a buffer no slot counts: every samples report stays uncounted.
        pool.take(ctx, (offset_slot.len() + extra) as u64 * 8)
    };
    if let Some(m) = mesh.as_mut() {
        let room = buffer.as_ref().map_or(0, |b| (b.len() / 8) as usize);
        m.bind(offset_slot.len(), room.saturating_sub(offset_slot.len()));
    }
    if buffer.is_none() {
        draw_slot.iter_mut().for_each(|d| *d = None);
        pass_offsets.clear();
    }
    Some(Arc::new(FrameQueries {
        counters: c.clone(),
        pool: pool.clone(),
        buffer,
        pass_offsets,
        offset_slot,
        draw_slot,
        expected,
        encoded: (0..slots).map(|_| AtomicU32::new(0)).collect(),
        cpu,
        mesh,
    }))
}

/// The vertices of a draw, None for an indirect or mesh draw.
fn vertices(args: &DrawArgs) -> Option<u64> {
    let (count, instances) = match *args {
        DrawArgs::Arrays {
            count, instances, ..
        }
        | DrawArgs::Indexed {
            count, instances, ..
        } => (count, instances),
        _ => return None,
    };
    Some(u64::from(count) * u64::from(instances.max(1)))
}

impl FrameQueries {
    /// The visibility offsets of render pass `index`, None when none of its draws counts samples.
    pub(crate) fn pass(&self, index: usize) -> Option<PassVis<'_>> {
        let buffer = self.buffer.as_deref()?;
        let offsets = self.pass_offsets.get(&index).map_or(&[][..], Vec::as_slice);
        if offsets.is_empty() && !self.mesh.as_ref().is_some_and(|m| m.has_pass(index)) {
            return None;
        }
        Some(PassVis { buffer, offsets })
    }

    /// The byte offset that mesh draw `n` (an ordinal) counts into for `NXKIT_DIAG=meshvis`. The caller asks only
    /// for a draw that has no slot of the reports.
    pub(crate) fn mesh_offset(&self, n: u32) -> Option<u64> {
        self.mesh.as_ref()?.offset(n)
    }

    /// The state that mesh draw `n` really used.
    pub(crate) fn mesh_note(&self, n: u32, p: &crate::draws::Prep) {
        if let Some(m) = &self.mesh {
            m.note(n, p);
        }
    }

    /// A Metal draw of mesh draw `n` was encoded with its offset set. Nothing for a draw that a report counts.
    pub(crate) fn mesh_encoded(&self, n: u32) {
        if let Some(m) = self.mesh.as_ref().filter(|_| self.draw_slot(n).is_none()) {
            m.encoded(n);
        }
    }

    /// True when the frame has mesh draws to print (`NXKIT_DIAG=meshvis`).
    pub(crate) fn has_mesh(&self) -> bool {
        self.mesh.is_some()
    }

    /// Prints the counts of the mesh draws. The GPU has to be done with the frame.
    pub(crate) fn mesh_print(&self, guest: &GuestSpace) {
        let Some(m) = &self.mesh else { return };
        let read = |i: u64| {
            let mut b = [0u8; 8];
            self.buffer.as_ref()?.read(i * 8, &mut b).ok()?;
            Some(u64::from_le_bytes(b))
        };
        let record = |off: u64| {
            let mut b = [0u8; 4];
            guest.read(off, &mut b).ok()?;
            Some(u32::from_le_bytes(b))
        };
        for l in m.report(&|n| self.draw_slot(n).is_some(), &read, &record) {
            eprintln!("{l}");
        }
    }

    /// The visibility result buffer, for the command buffer to keep alive.
    pub(crate) fn buffer(&self) -> Option<&Arc<DataBuffer>> {
        self.buffer.as_ref()
    }

    /// The slot that counts the samples of draw `n` (an ordinal).
    pub(crate) fn draw_slot(&self, n: u32) -> Option<u32> {
        self.draw_slot.get(n as usize).copied().flatten()
    }

    /// The encoder encoded a draw of `slot`.
    pub(crate) fn encoded(&self, slot: u32) {
        if let Some(e) = self.encoded.get(slot as usize) {
            e.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The samples of each slot, None for a slot whose draws were not all encoded.
    fn samples(&self) -> Vec<Option<u64>> {
        let mut sums: Vec<Option<u64>> = self
            .expected
            .iter()
            .zip(&self.encoded)
            .map(|(e, n)| (n.load(Ordering::Relaxed) == *e).then_some(0))
            .collect();
        for (o, slot) in self.offset_slot.iter().enumerate() {
            let mut b = [0u8; 8];
            let v = self
                .buffer
                .as_ref()
                .and_then(|buf| buf.read(o as u64 * 8, &mut b).ok())
                .map(|()| u64::from_le_bytes(b));
            if let Some(s) = sums.get_mut(*slot as usize) {
                *s = s.zip(v).map(|(a, b)| a + b);
            }
        }
        sums
    }

    /// Writes the reports into guest memory and lands them. `ticks` is the GPU end time of the frame. Runs after the
    /// GPU finished the frame.
    pub(crate) fn write(&self, guest: &GuestSpace, ticks: u64) -> WriteStats {
        let mut st = WriteStats::default();
        let samples = self.buffer.as_ref().map(|_| self.samples());
        let map = guest.address_map();
        for w in &self.counters.writes {
            let src = source(w.kind);
            let slot = |s: &u32| -> Option<u64> {
                let s = *s as usize;
                match src {
                    Source::Samples => samples.as_ref()?.get(s).copied().flatten(),
                    Source::Vertices => self.cpu.get(s)?.vertices,
                    Source::Primitives => self.cpu.get(s)?.primitives,
                    Source::Timestamp | Source::Unknown => None,
                }
            };
            let value = self
                .counters
                .sums_of(w)
                .iter()
                .try_fold(0u64, |a, s| Some(a.wrapping_add(slot(s)?)));
            let Some(value) = value else {
                st.uncounted += 1;
                continue;
            };
            if write_report(guest, &map, w.addr, value, ticks).is_none() {
                st.refused += 1;
                continue;
            }
            if let Some(l) = &self.counters.landed {
                l.land(w.addr, w.place, value, w.seq);
            }
            st.written += 1;
            if src == Source::Samples {
                st.samples += 1;
            } else {
                st.cpu += 1;
            }
        }
        st
    }
}

/// Writes a 16-byte `NVNcounterData` at the GPU address `addr`: the 64-bit counter, then the 64-bit timestamp, little
/// endian, as the GPU writes a four-word report. Only when all 16 bytes lie in guest memory; None otherwise.
pub(crate) fn write_report(
    guest: &GuestSpace,
    map: &nxkit_metal::AddressMap,
    addr: u64,
    counter: u64,
    ticks: u64,
) -> Option<()> {
    // The host address is checked against the guest range (all 16 bytes), then the offset again by `write`.
    let host = map.host_address(addr, 16).ok()?;
    let off = map.offset_of_host(host, 16).ok()?;
    let mut data = [0u8; 16];
    data[..8].copy_from_slice(&counter.to_le_bytes());
    data[8..].copy_from_slice(&ticks.to_le_bytes());
    guest.write(off, &data).ok()
}

/// `NXKIT_DIAG=rendercond`: what the writes of the finished frames did (the first 30 lines, then every 120th).
pub(crate) fn write_diag(st: &WriteStats) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ON.get_or_init(|| crate::diag("rendercond")) {
        return;
    }
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    if n < 30 || n.is_multiple_of(120) {
        eprintln!(
            "DIAG rendercond counters landed {} (samples {} vertices/primitives {}), not counted {} (a draw not encoded or indirect), outside guest memory {}",
            st.written, st.samples, st.cpu, st.uncounted, st.refused
        );
    }
}

#[cfg(test)]
mod tests;
