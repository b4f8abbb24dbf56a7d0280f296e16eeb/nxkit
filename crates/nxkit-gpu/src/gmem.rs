//! Raw global memory: the pool table (`gmem_map`) and the write log (`gmem_log`) of the translated shaders.
//!
//! A guest shader that loads or stores at an address it read from memory has no storage buffer to go through. The
//! translator (`nxb-xlate` `opt/gts_raw.rs`) then looks the address up in a table of the memory pools and accesses
//! the host address that the entry gives. Each store also sets the bit of its table entry in a write log. This module
//! builds both for the host. The layout is the one of the engine's `dmsh_abi.h` (`DMSH_GMEM_*`) and of
//! `nxb-xlate` `msl/abi.rs`.
//!
//! # The table
//!
//! `ENTRIES` entries of `ENTRY_BYTES` (32) bytes, sorted by start and disjoint:
//!
//! | byte | field |
//! |---|---|
//! | 0 | start of the range, u64, guest GPU address |
//! | 8 | end of the range, u64, exclusive |
//! | 16 | delta, u64: the address the GPU reads is `guest address + delta` (wrapping) |
//! | 24 | hint, u32: `chunk << 13 \| entry index` for the 2 MiB chunk `chunk & 8191`, 0 for none |
//! | 28 | padding |
//!
//! Entry 0 is empty (start 0, end 0), so the binary search of the shader always finds an entry. Unused entries after
//! the pools start at `!0`. A pool that overlaps an earlier one keeps only the part past that one's end: every pool is
//! a window of the same guest memory, so any pool that holds an address gives the right host address.
//!
//! The hint of the entry `chunk & 8191` names the entry that holds the whole 2 MiB chunk `chunk`. The shader checks the
//! chunk tag, so a colliding chunk falls back to the search.
//!
//! # The log
//!
//! A bitmap of `ENTRIES` bits: bit `i` of the u32 word `i / 32` is set by a shader that stored through entry `i`. Each
//! draw or dispatch with such a binding gets its own log, cleared, from a ring of logs that belongs to the batch. When
//! the batch slot comes round again (the batch ring has waited for the GPU), the logs are read and the pools that
//! shaders wrote are learned. This is a diagnostic and a hint: guest memory is the buffer itself, so a store through
//! the table reaches it at once and no copy has to follow.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use nxkit_metal::{BATCHES_IN_FLIGHT, Context, DataBuffer};
use nxkit_nvn::diag::{Reason, note, note_n};

/// Entries of the table (`GlobalToStorage.GmemMapLog2` = 13).
pub(crate) const ENTRIES: usize = 8192;
/// Bytes of one entry.
pub(crate) const ENTRY_BYTES: usize = 32;
/// Bytes of the table.
pub(crate) const TABLE_BYTES: u64 = (ENTRIES * ENTRY_BYTES) as u64;
/// The hint covers chunks of `1 << HINT_SHIFT` bytes.
pub(crate) const HINT_SHIFT: u32 = 21;
/// Bits of the entry index in a hint.
const HINT_INDEX_BITS: u32 = 13;
/// Bytes of one write log: one bit per entry.
pub(crate) const LOG_BYTES: u64 = (ENTRIES / 8) as u64;
/// Logs a batch can hand out. Later requests share the last one.
pub(crate) const LOGS_PER_BATCH: u32 = 1024;

/// The table with the ranges of its entries, as the log decoder needs them.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Table {
    /// The bytes to upload, `TABLE_BYTES` long.
    pub(crate) bytes: Vec<u8>,
    /// `(start, end)` of entry `i`. Entry 0 is `(0, 0)`. Only the used entries are listed.
    pub(crate) ranges: Vec<(u64, u64)>,
    /// Pools left out because the table is full.
    pub(crate) over: u32,
}

fn put_u64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

/// Builds the table of the pools `(start, end, delta)`, which must be sorted by start.
pub(crate) fn build_table(pools: impl IntoIterator<Item = (u64, u64, u64)>) -> Table {
    let mut bytes = vec![0u8; TABLE_BYTES as usize];
    for k in 1..ENTRIES {
        bytes[k * ENTRY_BYTES..k * ENTRY_BYTES + 8].fill(0xff);
    }
    let mut ranges = vec![(0u64, 0u64)];
    let (mut end, mut over) = (0u64, 0u32);
    for (start, hi, delta) in pools {
        let lo = start.max(end);
        if lo >= hi {
            continue;
        }
        if ranges.len() == ENTRIES {
            over += 1;
            continue;
        }
        let at = ranges.len() * ENTRY_BYTES;
        put_u64(&mut bytes, at, lo);
        put_u64(&mut bytes, at + 8, hi);
        put_u64(&mut bytes, at + 16, delta);
        ranges.push((lo, hi));
        end = hi;
    }
    // Each 2 MiB chunk that lies wholly inside an entry gets a hint. A later entry replaces the hint of a colliding
    // chunk, and the shader's tag check sends the other chunk to the search.
    for (k, &(lo, hi)) in ranges.iter().enumerate().skip(1) {
        let size = 1u64 << HINT_SHIFT;
        let (first, last) = (lo.div_ceil(size), hi >> HINT_SHIFT);
        for c in first..last.min(1 << (32 - HINT_INDEX_BITS)) {
            let hint = ((c << HINT_INDEX_BITS) | k as u64) as u32;
            let at = (c as usize % ENTRIES) * ENTRY_BYTES + 24;
            bytes[at..at + 4].copy_from_slice(&hint.to_le_bytes());
        }
    }
    Table {
        bytes,
        ranges,
        over,
    }
}

/// The memory pools of the game, and the table built from them.
pub(crate) struct Gmem {
    /// `start -> end` of each pool, in guest GPU addresses. Pools do not overlap.
    pools: BTreeMap<u64, u64>,
    /// Counts the changes of `pools`. The table is built again when it differs from `built`.
    generation: u64,
    built: u64,
    buffer: Option<Arc<DataBuffer>>,
    ranges: Arc<Vec<(u64, u64)>>,
    logs: Arc<GmemLogs>,
    /// The pools that logs showed written, sorted by start.
    learned: Vec<(u64, u64)>,
}

impl Gmem {
    pub(crate) fn new(ctx: &Context) -> Result<Self, nxkit_metal::MetalError> {
        Ok(Self {
            pools: BTreeMap::new(),
            generation: 1,
            built: 0,
            buffer: None,
            ranges: Arc::new(Vec::new()),
            logs: Arc::new(GmemLogs::new(ctx)?),
            learned: Vec::new(),
        })
    }

    /// A pool appeared. It replaces the pools it overlaps.
    pub(crate) fn add_pool(&mut self, start: u64, len: u64) {
        let end = start.saturating_add(len);
        if len == 0 {
            return;
        }
        let stale: Vec<u64> = self
            .pools
            .range(..end)
            .filter(|(s, e)| **s < end && **e > start)
            .map(|(s, _)| *s)
            .collect();
        for s in stale {
            self.pools.remove(&s);
        }
        self.pools.insert(start, end);
        self.generation += 1;
    }

    /// A pool went away.
    pub(crate) fn remove_pool(&mut self, start: u64) {
        if self.pools.remove(&start).is_some() {
            self.generation += 1;
        }
    }

    /// Builds the table again when the pools changed. `delta` gives the delta of a pool range, or `None` when the GPU
    /// cannot reach it (the pool is then left out). Returns the table's buffer, which the batch must keep alive.
    pub(crate) fn refresh(
        &mut self,
        ctx: &Context,
        delta: impl Fn(u64, u64) -> Option<u64>,
    ) -> Option<Arc<DataBuffer>> {
        if self.built != self.generation {
            let t = build_table(
                self.pools
                    .iter()
                    .filter_map(|(s, e)| delta(*s, *e - *s).map(|d| (*s, *e, d))),
            );
            if t.over != 0 {
                note_n(Reason::GmemPoolsOver, u64::from(t.over));
            }
            match DataBuffer::new(ctx, TABLE_BYTES).and_then(|b| b.write(0, &t.bytes).map(|()| b)) {
                Ok(b) => {
                    self.buffer = Some(Arc::new(b));
                    self.ranges = Arc::new(t.ranges);
                    if crate::diag("gmem") {
                        eprintln!(
                            "DIAG gmem table of {} ranges (pools {}, over {})",
                            self.ranges.len() - 1,
                            self.pools.len(),
                            t.over
                        );
                    }
                }
                Err(e) => {
                    note(Reason::GmemPoolsOver, format_args!("table buffer: {e:?}"));
                    self.buffer = None;
                }
            }
            self.built = self.generation;
        }
        self.buffer.clone()
    }

    /// The logs of the batches.
    pub(crate) fn logs(&self) -> &Arc<GmemLogs> {
        &self.logs
    }

    /// The ranges of the table the current batch binds.
    pub(crate) fn ranges(&self) -> Arc<Vec<(u64, u64)>> {
        self.ranges.clone()
    }

    /// The pools that shaders wrote through raw pointers, as far as the logs of finished batches showed.
    pub(crate) fn learned(&self) -> &[(u64, u64)] {
        &self.learned
    }

    /// Reads the logs of the batch that used `region` before and learns its pools. Call it when the batch ring has
    /// waited for that batch.
    pub(crate) fn learn(&mut self, region: usize) {
        for r in self.logs.drain(region) {
            if let Err(at) = self.learned.binary_search(&r) {
                self.learned.insert(at, r);
                note(
                    Reason::GmemPoolWritten,
                    format_args!("{:#x}..{:#x}", r.0, r.1),
                );
                if crate::diag("gmem") {
                    eprintln!(
                        "DIAG gmem shaders wrote the pool {:#x}..{:#x} ({} learned)",
                        r.0,
                        r.1,
                        self.learned.len()
                    );
                }
            }
        }
    }
}

/// The ranges of a table, shared with the logs that index it.
type Ranges = Arc<Vec<(u64, u64)>>;

/// The ring of write logs: `LOGS_PER_BATCH` logs for each batch slot.
pub(crate) struct GmemLogs {
    buffer: DataBuffer,
    used: [AtomicU32; BATCHES_IN_FLIGHT],
    /// The table each region's logs index.
    tables: Mutex<[Option<Ranges>; BATCHES_IN_FLIGHT]>,
}

impl GmemLogs {
    fn new(ctx: &Context) -> Result<Self, nxkit_metal::MetalError> {
        Ok(Self {
            buffer: DataBuffer::new(
                ctx,
                LOG_BYTES * u64::from(LOGS_PER_BATCH) * BATCHES_IN_FLIGHT as u64,
            )?,
            used: Default::default(),
            tables: Mutex::new(Default::default()),
        })
    }

    /// Starts the logs of `region` against `table`. The region must be drained.
    pub(crate) fn begin(&self, region: usize, table: Arc<Vec<(u64, u64)>>) {
        self.used[region].store(0, Ordering::Release);
        self.tables.lock().unwrap_or_else(PoisonError::into_inner)[region] = Some(table);
    }

    /// The GPU address of a zeroed log for one draw or dispatch of `region`. Past the last log, the last one is shared.
    pub(crate) fn take(&self, region: usize) -> u64 {
        let n = self.used[region].fetch_add(1, Ordering::AcqRel);
        let slot = if n >= LOGS_PER_BATCH {
            note(Reason::GmemLogsShared, format_args!("region {region}"));
            LOGS_PER_BATCH - 1
        } else {
            n
        };
        self.buffer.gpu_address()
            + (region as u64 * u64::from(LOGS_PER_BATCH) + u64::from(slot)) * LOG_BYTES
    }

    /// Reads and clears the logs of `region`. Returns the ranges of the entries with a bit set, sorted.
    pub(crate) fn drain(&self, region: usize) -> Vec<(u64, u64)> {
        let used = self.used[region]
            .swap(0, Ordering::AcqRel)
            .min(LOGS_PER_BATCH);
        let table = self.tables.lock().unwrap_or_else(PoisonError::into_inner)[region].take();
        let mut seen = vec![0u32; ENTRIES / 32];
        let base = region as u64 * u64::from(LOGS_PER_BATCH) * LOG_BYTES;
        let mut log = vec![0u8; LOG_BYTES as usize];
        let zero = vec![0u8; LOG_BYTES as usize];
        for i in 0..u64::from(used) {
            let at = base + i * LOG_BYTES;
            if self.buffer.read(at, &mut log).is_err() {
                break;
            }
            for (w, c) in seen.iter_mut().zip(log.chunks_exact(4)) {
                *w |= u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
            }
            let _ = self.buffer.write(at, &zero);
        }
        decode(&seen, table.as_deref().map_or(&[][..], Vec::as_slice))
    }
}

/// The ranges of the entries whose bit is set in `words`. Entry 0 and entries past the table are ignored.
pub(crate) fn decode(words: &[u32], ranges: &[(u64, u64)]) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    for (w, bits) in words.iter().enumerate() {
        let mut rest = *bits;
        while rest != 0 {
            let i = w * 32 + rest.trailing_zeros() as usize;
            rest &= rest - 1;
            if i != 0
                && let Some(r) = ranges.get(i)
            {
                out.push(*r);
            }
        }
    }
    out
}

/// What the argument buffers of a frame need of the raw global memory.
#[derive(Clone, Copy)]
pub(crate) struct GmemEnv<'a> {
    /// The GPU address of the table, 0 when it could not be built.
    pub(crate) map: u64,
    /// The logs, and the region of the batch.
    pub(crate) logs: Option<(&'a GmemLogs, usize)>,
}

impl GmemEnv<'_> {
    /// An environment without a table, for tests.
    pub(crate) const NONE: GmemEnv<'static> = GmemEnv { map: 0, logs: None };

    /// The address of a log for one draw or dispatch, 0 when there are no logs.
    pub(crate) fn log(&self) -> u64 {
        self.logs.map_or(0, |(l, r)| l.take(r))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u64_at(b: &[u8], at: usize) -> u64 {
        u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
    }

    fn u32_at(b: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn the_table_has_the_layout_of_the_abi() {
        let mib = 1u64 << 20;
        let t = build_table([
            (0x10 * mib, 0x14 * mib, 0),
            (0x40 * mib, 0x41 * mib, 0x1000),
        ]);
        assert_eq!(t.bytes.len(), 8192 * 32);
        // entry 0 is empty, unused entries start at !0
        assert_eq!(&t.bytes[..32], &[0u8; 32]);
        assert_eq!(u64_at(&t.bytes, 3 * 32), u64::MAX);
        assert_eq!(u64_at(&t.bytes, 3 * 32 + 8), 0);
        // entry 1 and 2: start, end, delta
        assert_eq!(
            (
                u64_at(&t.bytes, 32),
                u64_at(&t.bytes, 40),
                u64_at(&t.bytes, 48)
            ),
            (0x10 * mib, 0x14 * mib, 0)
        );
        assert_eq!(
            (
                u64_at(&t.bytes, 64),
                u64_at(&t.bytes, 72),
                u64_at(&t.bytes, 80)
            ),
            (0x40 * mib, 0x41 * mib, 0x1000)
        );
        assert_eq!(
            t.ranges,
            vec![(0, 0), (0x10 * mib, 0x14 * mib), (0x40 * mib, 0x41 * mib)]
        );
    }

    #[test]
    fn an_overlapping_pool_keeps_the_part_past_the_earlier_one() {
        let t = build_table([(100, 200, 1), (150, 300, 2), (120, 180, 3), (400, 400, 4)]);
        // the third pool is wholly inside the second range, the fourth is empty
        assert_eq!(t.ranges, vec![(0, 0), (100, 200), (200, 300)]);
        assert_eq!(u64_at(&t.bytes, 2 * 32 + 16), 2);
    }

    #[test]
    fn hints_name_the_entry_that_holds_a_whole_chunk() {
        let size = 1u64 << HINT_SHIFT;
        // chunks 3 and 4 lie inside [2.5 chunks, 5 chunks); chunk 2 is cut by the start
        let t = build_table([(size * 5 / 2, size * 5, 0)]);
        let hint_of = |c: u64| u32_at(&t.bytes, (c as usize % ENTRIES) * ENTRY_BYTES + 24);
        assert_eq!(hint_of(2), 0);
        assert_eq!(hint_of(3), 3 << 13 | 1);
        assert_eq!(hint_of(4), 4 << 13 | 1);
        assert_eq!(hint_of(5), 0);
    }

    #[test]
    fn a_full_table_counts_the_pools_it_leaves_out() {
        let t = build_table((0..ENTRIES as u64 + 5).map(|i| (i * 0x1000, i * 0x1000 + 0x800, 0)));
        assert_eq!(t.ranges.len(), ENTRIES);
        assert_eq!(t.over, 6);
    }

    #[test]
    fn the_log_decodes_to_the_ranges_of_the_set_bits() {
        let ranges = vec![(0, 0), (10, 20), (30, 40), (50, 60)];
        let mut words = vec![0u32; ENTRIES / 32];
        words[0] = 0b1111;
        // a bit past the table is ignored
        words[3] = 1 << 7;
        assert_eq!(decode(&words, &ranges), vec![(10, 20), (30, 40), (50, 60)]);
        assert_eq!(LOG_BYTES, 1024);
    }

    #[test]
    fn pools_replace_the_pools_they_overlap() {
        let Ok(ctx) = Context::system_default() else {
            return;
        };
        let mut g = Gmem::new(&ctx).unwrap();
        g.add_pool(0x1000, 0x1000);
        g.add_pool(0x1800, 0x1000);
        assert_eq!(g.pools.len(), 1);
        g.remove_pool(0x1800);
        assert!(g.pools.is_empty());
    }

    #[test]
    fn logs_come_back_through_the_table_of_their_batch() {
        let Ok(ctx) = Context::system_default() else {
            return;
        };
        let g = Gmem::new(&ctx).unwrap();
        let logs = g.logs();
        logs.begin(1, Arc::new(vec![(0, 0), (0x100, 0x200), (0x300, 0x400)]));
        let a = logs.take(1);
        let b = logs.take(1);
        assert_eq!(b - a, LOG_BYTES);
        // the shader of the second draw stored through entry 2
        logs.buffer
            .write(b - logs.buffer.gpu_address(), &4u32.to_le_bytes())
            .unwrap();
        assert_eq!(logs.drain(1), vec![(0x300, 0x400)]);
        // drained and cleared
        assert!(logs.drain(1).is_empty());
        assert_eq!(logs.take(1), a);
    }
}
