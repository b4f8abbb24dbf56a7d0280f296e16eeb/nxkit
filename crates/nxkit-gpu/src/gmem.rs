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
//! | 16 | delta, u64: the address the GPU reads is `guest address + delta` (wrapping); the real address of the pool minus its guest address, so it is not 0 and it may be negative |
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

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use nxkit_metal::{BATCHES_IN_FLIGHT, Context, DataBuffer};
use nxkit_nvn::diag::{Reason, note, note_n};

mod registry;

use self::registry::{Registry, count_overlapping};
use crate::pools::{Backing, PoolSnapshot};

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

/// Builds the table of the pools `(start, end, delta)`, which must be sorted by start, the longer pool first for equal
/// starts. A pool that lies inside the ranges before it adds nothing.
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

/// Gives each registration its delta from the snapshot. Returns the pools that resolve and the number that do not.
fn resolve_all<B: Backing>(
    sorted: &[(u64, u64)],
    snapshot: &PoolSnapshot<B>,
) -> (Vec<(u64, u64, u64)>, u32) {
    let mut unresolved = 0u32;
    let pools = sorted
        .iter()
        .filter_map(|&(s, e)| match snapshot.resolve(s, e - s) {
            Ok(at) => Some((s, e, at.real.wrapping_sub(s))),
            Err(_) => {
                unresolved += 1;
                None
            }
        })
        .collect();
    (pools, unresolved)
}

/// The memory pools of the game, and the table built from them.
pub(crate) struct Gmem {
    /// Every registered pool until it is removed. The table is the union of them.
    pools: Registry,
    /// Counts the changes of `pools`.
    generation: u64,
    /// The generation of `pools` and the generation of the pool snapshot that the table was built from. The table is
    /// built again when either differs.
    built: Option<(u64, u64)>,
    buffer: Option<Arc<DataBuffer>>,
    ranges: Arc<Vec<(u64, u64)>>,
    logs: Arc<GmemLogs>,
    /// The pools that logs showed written, sorted by start.
    learned: Vec<(u64, u64)>,
}

impl Gmem {
    pub(crate) fn new(ctx: &Context) -> Result<Self, nxkit_metal::MetalError> {
        Ok(Self {
            pools: Registry::default(),
            generation: 1,
            built: None,
            buffer: None,
            ranges: Arc::new(Vec::new()),
            logs: Arc::new(GmemLogs::new(ctx)?),
            learned: Vec::new(),
        })
    }

    /// A pool appeared. It is added to the pools it overlaps; none of them is dropped.
    pub(crate) fn add_pool(&mut self, start: u64, len: u64) {
        if self.pools.add(start, len) {
            self.generation += 1;
        }
    }

    /// A pool went away. The oldest registration with this start goes: a newer pool at the same start stays.
    pub(crate) fn remove_pool(&mut self, start: u64) {
        if self.pools.remove(start) {
            self.generation += 1;
        }
    }

    /// Builds the table again when the pools or the snapshot changed. The delta of a pool is the snapshot's real
    /// address of its start minus the start; a pool that the snapshot does not resolve is left out and counted
    /// (`gmem.pool-unresolved`). Returns the
    /// table's buffer, which the batch must keep alive.
    pub(crate) fn refresh<B: Backing>(
        &mut self,
        ctx: &Context,
        snapshot: &PoolSnapshot<B>,
    ) -> Option<Arc<DataBuffer>> {
        let stamp = (self.generation, snapshot.generation());
        if self.built != Some(stamp) {
            let sorted = self.pools.sorted();
            let overlapping = count_overlapping(&sorted);
            let (resolved, unresolved) = resolve_all(&sorted, snapshot);
            let t = build_table(resolved);
            if unresolved != 0 {
                note_n(Reason::GmemPoolUnresolved, u64::from(unresolved));
            }
            if t.over != 0 {
                note_n(Reason::GmemPoolsOver, u64::from(t.over));
            }
            match DataBuffer::new(ctx, TABLE_BYTES).and_then(|b| b.write(0, &t.bytes).map(|()| b)) {
                Ok(b) => {
                    self.buffer = Some(Arc::new(b));
                    self.ranges = Arc::new(t.ranges);
                    if crate::diag("gmem") {
                        eprintln!(
                            "DIAG gmem table of {} ranges (pools {}, overlapping {}, unresolved {}, over {})",
                            self.ranges.len() - 1,
                            sorted.len(),
                            overlapping,
                            unresolved,
                            t.over
                        );
                    }
                }
                Err(e) => {
                    note(Reason::GmemPoolsOver, format_args!("table buffer: {e:?}"));
                    self.buffer = None;
                }
            }
            self.built = Some(stamp);
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
            for (w, c) in seen.iter_mut().zip(log.as_chunks::<4>().0) {
                *w |= u32::from_le_bytes(*c);
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
    #[cfg(test)]
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
    fn the_pools_the_snapshot_does_not_resolve_are_counted() {
        let set = shifted_set();
        set.register(0x4000, 0x4000, 0).unwrap();
        let sorted = [
            (0x4000, 0x8000),
            (0x10_0000, 0x10_1000),
            (0x20_0000, 0x20_1000),
        ];
        let (resolved, unresolved) = resolve_all(&sorted, &*set.current());
        assert_eq!(resolved, vec![(0x4000, 0x8000, SHIFT)]);
        assert_eq!(unresolved, 2);
    }

    /// A backing whose real address is a fixed distance from the host address, so a table delta is visible.
    struct Shifted(u64);

    impl Backing for Shifted {
        fn real_base(&self) -> u64 {
            self.0
        }
    }

    struct NoPins;

    impl crate::pools::PagePins for NoPins {
        fn pin(&self, _start: u64, _end: u64) {}
        fn unpin(&self, _start: u64, _end: u64) {}
    }

    const SHIFT: u64 = 0x1_0000_0000;

    fn shifted_set() -> crate::pools::PoolSet<Shifted> {
        crate::pools::PoolSet::new(
            0,
            0,
            |host: u64, _len: u64| Ok(Shifted(host + SHIFT)),
            Arc::new(NoPins),
        )
    }

    fn delta_of_entry(buf: &DataBuffer, entry: usize) -> u64 {
        let mut b = [0u8; 8];
        buf.read((entry * ENTRY_BYTES + 16) as u64, &mut b).unwrap();
        u64::from_le_bytes(b)
    }

    #[test]
    fn the_table_takes_the_delta_of_a_pool_from_the_snapshot() {
        let Ok(ctx) = Context::system_default() else {
            return;
        };
        let set = shifted_set();
        set.register(0x4000, 0x4000, 0).unwrap();
        let mut g = Gmem::new(&ctx).unwrap();
        g.add_pool(0x4000, 0x4000);
        // a pool that the snapshot does not hold is left out
        g.add_pool(0x10_0000, 0x1000);
        let buf = g.refresh(&ctx, &*set.current()).unwrap();
        assert_eq!(g.ranges(), Arc::new(vec![(0, 0), (0x4000, 0x8000)]));
        assert_eq!(delta_of_entry(&buf, 1), SHIFT);
    }

    #[test]
    fn the_delta_of_the_whole_mode_is_the_real_address_minus_the_virtual_one() {
        use nxkit_metal::GUEST_VA_BASE;
        let Ok(ctx) = Context::system_default() else {
            return;
        };
        // The real address of the buffer lies below the window, so the delta is negative and wraps.
        let real = 0x15_0000_0000u64;
        let snap = PoolSnapshot::whole(GUEST_VA_BASE, 0x10_0000, Shifted(real));
        let mut g = Gmem::new(&ctx).unwrap();
        let start = GUEST_VA_BASE + 0x4000;
        g.add_pool(start, 0x4000);
        let buf = g.refresh(&ctx, &snap).unwrap();
        assert_eq!(g.ranges(), Arc::new(vec![(0, 0), (start, start + 0x4000)]));
        let delta = delta_of_entry(&buf, 1);
        assert_eq!(delta, real.wrapping_sub(GUEST_VA_BASE));
        assert_ne!(delta, 0);
        // The shader adds the delta to the guest address, modulo 2^64.
        assert_eq!((start + 0x10).wrapping_add(delta), real + 0x4010);
    }

    #[test]
    fn the_whole_window_fits_the_chunk_numbers_of_the_hints() {
        use nxkit_metal::{GUEST_VA_BASE, GUEST_VA_MAX};
        // A hint holds 19 bits of the chunk number, so the last chunk of the window must fit.
        const { assert!((GUEST_VA_MAX - 1) >> HINT_SHIFT < 1 << (32 - HINT_INDEX_BITS)) };
        let size = 1u64 << HINT_SHIFT;
        let t = build_table([(GUEST_VA_BASE, GUEST_VA_BASE + 4 * size, 7)]);
        let c = GUEST_VA_BASE >> HINT_SHIFT;
        let hint = u32_at(&t.bytes, (c as usize % ENTRIES) * ENTRY_BYTES + 24);
        assert_eq!(u64::from(hint), c << 13 | 1);
    }

    #[test]
    fn the_table_is_built_again_only_when_a_generation_changed() {
        let Ok(ctx) = Context::system_default() else {
            return;
        };
        let set = shifted_set();
        set.register(0x4000, 0x4000, 0).unwrap();
        let mut g = Gmem::new(&ctx).unwrap();
        g.add_pool(0x4000, 0x4000);
        let snap = set.current();
        let first = g.refresh(&ctx, &*snap).unwrap();
        // the same pools and the same generation: the same buffer
        let again = g.refresh(&ctx, &*snap).unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        let same = g.refresh(&ctx, &*set.current()).unwrap();
        assert!(Arc::ptr_eq(&first, &same));
        // a new snapshot generation builds it again
        set.register(0x10_0000, 0x4000, 0).unwrap();
        let newer = g.refresh(&ctx, &*set.current()).unwrap();
        assert!(!Arc::ptr_eq(&first, &newer));
        let held = g.refresh(&ctx, &*set.current()).unwrap();
        assert!(Arc::ptr_eq(&newer, &held));
        // so does a change of the pools of the table
        g.add_pool(0x10_0000, 0x4000);
        let last = g.refresh(&ctx, &*set.current()).unwrap();
        assert!(!Arc::ptr_eq(&newer, &last));
        assert_eq!(g.ranges().len(), 3);
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
