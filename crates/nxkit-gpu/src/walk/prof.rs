//! `NXKIT_DIAG=walkprof`: where the time of one walk (one `nvnQueueSubmitCommands`) goes.
//!
//! The walker reads the records of the submit a second time, beside the decode. When it is longer than the decode, the
//! game thread waits for it. This splits the walk into the parts that can be made shorter:
//!
//! - `read`: `Record::read`, the decode of one record from guest memory.
//! - `apply`: the records that change the state (binds, state objects, clears, copies, updates), without draws.
//! - `detail`: building the `DrawDetail` of a draw, without its snapshots.
//! - `snap`: `Walker::snapshot`, the copies of the uniform and storage buffers (without the fresh allocations).
//! - `alloc`: fresh snapshot buffers and the `Arc` of each draw.
//!
//! The clock reads cost some time, so the walk is slower with the category on. The numbers show the share of each part.

use std::cell::Cell;
use std::time::Instant;

use super::State;

/// The counters of the walk since the last report. Everything is zero and unused while the category is off.
pub(super) struct Prof {
    on: bool,
    /// Submits reported so far.
    reported: Cell<u32>,
    pub(super) read_ns: Cell<u64>,
    pub(super) apply_ns: Cell<u64>,
    pub(super) detail_ns: Cell<u64>,
    pub(super) snap_ns: Cell<u64>,
    /// Fresh snapshot buffers, inside `snap_ns`.
    pub(super) alloc_snap_ns: Cell<u64>,
    /// The `Arc` of each draw.
    pub(super) alloc_draw_ns: Cell<u64>,
    /// Calls of `snapshot`.
    pub(super) snaps: Cell<u64>,
    /// Calls that the per-submit map answered.
    pub(super) snap_hits: Cell<u64>,
    /// Ranges that no update touched, so they carry no copy.
    pub(super) snap_none: Cell<u64>,
    /// Ranges that were copied into a buffer.
    pub(super) snap_copied: Cell<u64>,
    /// Ranges whose bytes equalled an earlier copy of the frame, so they share its buffer.
    pub(super) snap_shared: Cell<u64>,
    /// Snapshot buffers that the pool could not give.
    pub(super) snap_fresh: Cell<u64>,
    pub(super) snap_bytes: Cell<u64>,
    /// Heap blocks of the draws: one `Arc` and each vector that is not empty.
    pub(super) draw_blocks: Cell<u64>,
    /// Draw banks (uniform, storage, vertex input, textures) that equal the same bank of the draw before.
    pub(super) banks_same: Cell<u64>,
    pub(super) banks_total: Cell<u64>,
    /// The state of the draw before, while the category is on.
    prev: Option<Box<State>>,
}

impl Prof {
    pub(super) fn new() -> Self {
        Self {
            on: nxkit_nvn::diag::on("walkprof"),
            reported: Cell::new(0),
            read_ns: Cell::new(0),
            apply_ns: Cell::new(0),
            detail_ns: Cell::new(0),
            snap_ns: Cell::new(0),
            alloc_snap_ns: Cell::new(0),
            alloc_draw_ns: Cell::new(0),
            snaps: Cell::new(0),
            snap_hits: Cell::new(0),
            snap_none: Cell::new(0),
            snap_copied: Cell::new(0),
            snap_shared: Cell::new(0),
            snap_fresh: Cell::new(0),
            snap_bytes: Cell::new(0),
            draw_blocks: Cell::new(0),
            banks_same: Cell::new(0),
            banks_total: Cell::new(0),
            prev: None,
        }
    }

    pub(super) fn on(&self) -> bool {
        self.on
    }

    /// The start of a timed part, `None` while the category is off.
    #[inline]
    pub(super) fn start(&self) -> Option<Instant> {
        self.on.then(Instant::now)
    }

    /// Adds the time since `t` to `total`.
    #[inline]
    pub(super) fn lap(&self, total: &Cell<u64>, t: Option<Instant>) {
        if let Some(t) = t {
            total.set(total.get() + u64::try_from(t.elapsed().as_nanos()).unwrap_or(u64::MAX));
        }
    }

    /// Adds `n` to a counter while the category is on.
    #[inline]
    pub(super) fn count(&self, c: &Cell<u64>, n: u64) {
        if self.on {
            c.set(c.get() + n);
        }
    }

    /// Counts the heap blocks of a draw.
    pub(super) fn draw_built(&self, d: &crate::detail::DrawDetail) {
        if self.on {
            let vecs = [
                d.attributes.capacity(),
                d.streams.capacity(),
                d.ubos.capacity(),
                d.sbufs.capacity(),
                d.textures.capacity(),
            ];
            self.draw_blocks
                .set(self.draw_blocks.get() + 1 + vecs.iter().filter(|c| **c != 0).count() as u64);
        }
    }

    /// Counts how many banks of this draw equal those of the draw before: what a dirty bit per bank would save.
    pub(super) fn banks(&mut self, st: &State) {
        if !self.on {
            return;
        }
        if let Some(p) = &self.prev {
            let same = [
                p.ubo[0] == st.ubo[0],
                p.ubo[1] == st.ubo[1],
                p.sbuf[0] == st.sbuf[0],
                p.sbuf[1] == st.sbuf[1],
                p.attribute == st.attribute
                    && p.stream == st.stream
                    && p.vertex_buffer == st.vertex_buffer,
                p.texture[..32] == st.texture[..32],
                p.texture[32..64] == st.texture[32..64],
            ];
            self.banks_total
                .set(self.banks_total.get() + same.len() as u64);
            self.banks_same
                .set(self.banks_same.get() + same.iter().filter(|s| **s).count() as u64);
        }
        self.prev = Some(Box::new(st.clone()));
    }

    /// Prints the counters of one submit and starts them again. The first 200 submits print, then every 20th.
    pub(super) fn report(&self, records: u64, draws: usize, dispatches: usize, total_ns: u64) {
        if !self.on {
            return;
        }
        let n = self.reported.get();
        self.reported.set(n + 1);
        let print = n < 200 || n % 20 == 0;
        let g = |c: &Cell<u64>| c.replace(0);
        let us = |ns: u64| ns as f64 / 1000.0;
        let (read, apply, detail, snap, alloc_snap, alloc_draw) = (
            g(&self.read_ns),
            g(&self.apply_ns),
            g(&self.detail_ns),
            g(&self.snap_ns),
            g(&self.alloc_snap_ns),
            g(&self.alloc_draw_ns),
        );
        let counts = [
            g(&self.snaps),
            g(&self.snap_hits),
            g(&self.snap_none),
            g(&self.snap_copied),
            g(&self.snap_shared),
            g(&self.snap_fresh),
            g(&self.snap_bytes),
            g(&self.draw_blocks),
            g(&self.banks_same),
            g(&self.banks_total),
        ];
        if !print {
            return;
        }
        // `apply` holds the detail and the draw allocations, `detail` the snapshots and `snap` the fresh buffers, so each
        // part is shown without the parts inside it.
        let apply_own = apply.saturating_sub(detail + alloc_draw);
        let detail_own = detail.saturating_sub(snap);
        let snap_own = snap.saturating_sub(alloc_snap);
        let [
            snaps,
            hits,
            none,
            copied,
            shared,
            fresh,
            bytes,
            blocks,
            same,
            total,
        ] = counts;
        eprintln!(
            "DIAG walkprof submit {n}: total {:.0}us = read {:.0} + apply {:.0} + detail {:.0} + snap {:.0} + alloc {:.0} us \
             (rest {:.0}); records {records} draws {draws} dispatches {dispatches}; snapshots {snaps} \
             (map {hits}, uncopied {none}, copied {copied}, shared {shared}, fresh buffers {fresh}, {} KiB); \
             draw blocks {blocks}; banks equal to the draw before {same}/{total}",
            us(total_ns),
            us(read),
            us(apply_own),
            us(detail_own),
            us(snap_own),
            us(alloc_snap + alloc_draw),
            us(total_ns.saturating_sub(read + apply)),
            bytes / 1024,
        );
    }
}
