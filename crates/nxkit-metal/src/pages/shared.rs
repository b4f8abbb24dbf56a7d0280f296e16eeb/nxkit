//! `Shared`, what the signal handler reads, and its counters.

use super::*;

/// What the signal handler reads, and the counters.
pub(super) struct Shared {
    pub(super) base: AtomicU64,
    pub(super) len: AtomicU64,
    pub(super) page: AtomicU64,
    /// The table: `npages` words, zero for free.
    pub(super) table: AtomicPtr<AtomicU32>,
    pub(super) npages: AtomicUsize,
    /// The pages that are not free.
    pub(super) members: AtomicU64,
    /// The newest frame whose completion was reported.
    pub(super) completed: AtomicU64,
    /// The pages that went dirty since the last flush, as page number plus one; a slot is zero until its writer filled
    /// it.
    pub(super) log: [AtomicU32; LOG_CAP],
    pub(super) log_n: AtomicUsize,
    pub(super) internal: [AtomicUsize; INTERNAL],
    pub(super) old: [OldAction; 2],
    /// Faults by `[kind][origin]`.
    pub(super) faults: [[AtomicU64; 3]; 5],
    /// Time in the handler, for tracked pages.
    pub(super) handler_ns: AtomicU64,
    /// Accesses by system calls (the hook) to pages of a frame in flight: read, write.
    pub(super) host_access: [AtomicU64; 2],
    /// Pages dirtied by the hook rather than by a fault.
    pub(super) hook_dirtied: AtomicU64,
    /// Calls of `mprotect`, and the time of those outside the handler.
    pub(super) mprotects: AtomicU64,
    pub(super) mprotect_ns: AtomicU64,
    pub(super) mprotect_failed: AtomicU64,
    /// Pages the log could not hold.
    pub(super) log_overflows: AtomicU64,
    /// The last signal of a tracked fault.
    pub(super) last_signal: AtomicU64,
    /// The host address of the mirror of guest memory (byte `o` of guest memory is at `mirror + o`), 0 when there is no
    /// mirror. With a mirror, a page tagged with a frame holds stale guest bytes and the handler copies it.
    pub(super) mirror: AtomicU64,
    /// Bumped, and woken on, at each completion and each finished copy.
    pub(super) wake: AtomicU32,
    /// The newest frame whose flush ran, and the newest whose commit was reported. Where the newest frame is follows from
    /// them and `completed`: 1 flushed and not committed, 2 committed and running, 0 done. Writes that dirty a page are
    /// counted by it.
    pub(super) flushed: AtomicU64,
    pub(super) committed: AtomicU64,
    pub(super) phase_writes: [AtomicU64; 3],
    /// The addresses of the first writes to member pages between the flush of a frame and its commit, by frame number
    /// modulo [`P1_RING`], as the byte offset in guest memory plus one, and how many writes the frame logged (the log
    /// holds [`P1_CAP`]).
    pub(super) p1_n: [AtomicU32; P1_RING],
    pub(super) p1_log: [[AtomicU64; P1_CAP]; P1_RING],
    /// Waits of a thread for the writer of a page, by access (0 read, 1 write): count, time, the longest, and how many
    /// began before the frame was committed (the wait then includes the encode).
    pub(super) wait_n: [AtomicU64; 2],
    pub(super) wait_ns: [AtomicU64; 2],
    pub(super) wait_max_ns: [AtomicU64; 2],
    pub(super) wait_uncommitted: [AtomicU64; 2],
    /// The first access of a game thread to a page of a frame in flight, as `frame << 36 | write << 35 | page + 1`, and
    /// how many there were since the last [`Pages::take_in_flight`].
    pub(super) flight_n: AtomicU32,
    pub(super) flight_log: [AtomicU64; FLIGHT_CAP],
    /// With a mirror: copy every page of a completed frame back into guest memory at once (a bisecting aid).
    pub(super) eager: AtomicBool,
    /// Pages copied from the mirror into guest memory.
    pub(super) synced: AtomicU64,
    /// Waits for a writer that was still running, and their time.
    pub(super) writer_waits: AtomicU64,
    pub(super) writer_wait_ns: AtomicU64,
    /// Accesses of internal threads to pages in flight, served without waiting.
    pub(super) internal_opens: AtomicU64,
    /// Waits that ended at the timeout.
    pub(super) wait_timeouts: AtomicU64,
    /// The member bitmap (one bit per page, set while the page is not free) and its generation.
    pub(super) member_bits: AtomicPtr<AtomicU64>,
    pub(super) member_gen: AtomicU64,
    /// Program counters by [`FaultKind`] 0 to 2: the counter and how often it faulted.
    pub(super) pcs: [[(AtomicU64, AtomicU64); PC_SLOTS]; 5],
    pub(super) pc_overflow: AtomicU64,
    /// The per-page stale blocks and flags: `npages` entries.
    pub(super) info: AtomicPtr<PageInfo>,
    /// The address of the alias of the byte at `base` (a second mapping of guest memory with read and write access), 0
    /// when there is none.
    pub(super) alias: AtomicU64,
    /// The handler emulates accesses outside the stale blocks.
    pub(super) emulate: AtomicBool,
    /// Emulated accesses by form, the time they took in the handler, and the accesses that fell back by reason.
    pub(super) emu: [AtomicU64; Form::COUNT],
    pub(super) emu_ns: AtomicU64,
    pub(super) fallback: [AtomicU64; Reason::COUNT],
    /// Pages the flush staged because the handler stored to them.
    pub(super) emu_staged: AtomicU64,
    /// Step 5: the completion of a frame copies back, on its own thread, the pages that game threads faulted on for a
    /// copy lately; pages that the CPU writes in many flushes in a row stay writable and are diffed at each flush.
    pub(super) eager_on: AtomicBool,
    pub(super) hot_on: AtomicBool,
    /// The epoch (`frame / EPOCH_FRAMES`) of the newest flush.
    pub(super) epoch: AtomicU32,
    /// Pages the eager copy moved, the time it took, and the pages it found changed by someone else.
    pub(super) eager_pages: AtomicU64,
    pub(super) eager_ns: AtomicU64,
    pub(super) eager_runs: AtomicU64,
    /// Step 6: the pages a game thread waited for (page plus one), until [`Pages::readers`] takes them.
    pub(super) readers_log: [AtomicU32; READERS_CAP],
    pub(super) readers_n: AtomicUsize,
    /// Waits by what ended them (0 the pass signal of the page, the frame still running; 1 the completion of the whole
    /// frame) and their time.
    pub(super) wait_end: [AtomicU64; 2],
    pub(super) wait_end_ns: [AtomicU64; 2],
    /// Calls of [`Pages::release_early`], the pages they released, and the faults that found a page released while its frame
    /// ran.
    pub(super) early_calls: AtomicU64,
    pub(super) early_pages: AtomicU64,
    pub(super) early_syncs: AtomicU64,
    /// Results one frame late: the handler serves the CPU's accesses to the blocks a frame in flight writes from guest
    /// memory, which holds the last completed frame's bytes, without waiting.
    pub(super) late: AtomicBool,
    /// Results one frame late: the flush leaves a page readable when its guest bytes are a completed frame's.
    pub(super) read_open_on: AtomicBool,
    /// Late reads and writes, the age of the bytes they met (frames: the frame in flight minus the frame the bytes are
    /// from) as a sum, a histogram (1, 2, 3, 4 or more) and the largest since the last [`Pages::take_late_age_max`], the
    /// blocks the CPU took from the frame (written while it ran), the blocks a copy from the mirror left alone for that
    /// reason, the pages and blocks the completion of a frame copied for the next ones, and the pages the flush copied.
    pub(super) late_reads: AtomicU64,
    pub(super) late_writes: AtomicU64,
    pub(super) late_age_sum: AtomicU64,
    pub(super) late_age: [AtomicU64; 4],
    pub(super) late_age_max: AtomicU64,
    pub(super) late_owned: AtomicU64,
    pub(super) late_kept: AtomicU64,
    pub(super) late_copied: AtomicU64,
    pub(super) late_copied_blocks: AtomicU64,
    pub(super) late_flush_copies: AtomicU64,
    /// Exact copies: pages the completion copied from the GPU's copy of the frame's end, blocks of CPU writes the frame
    /// changed on the GPU (the frame's bytes win there), the CPU's bytes of those blocks carried to the next flush, and
    /// carried blocks that no flush staged (the page was released or stayed writable).
    pub(super) late_exact: AtomicU64,
    pub(super) late_frame_won: AtomicU64,
    pub(super) late_carried: AtomicU64,
    pub(super) late_carry_lost: AtomicU64,
    /// The newest frame whose completion finished its copies (staging buffers and the GPU's copies of a frame can be
    /// used again from then on).
    pub(super) settled: AtomicU64,
    /// Snapshots dropped, by [`SnapWhy`], and late accesses that found no snapshot, by the reason the page lost it last.
    pub(super) snap_resets: [AtomicU64; SnapWhy::COUNT],
    pub(super) snap_misses: [AtomicU64; SnapWhy::COUNT],
    /// Pages that the flush left readable while a frame in flight writes them (results one frame late: guest memory holds
    /// the last completed frame's bytes there), and the pages that lost this at a completion.
    pub(super) read_open: AtomicU64,
    pub(super) read_revoked: AtomicU64,
    /// System calls that read a page in flight outside its stale blocks without a wait, and the ones that waited for a writer.
    pub(super) settle_late: AtomicU64,
    pub(super) settle_waits: AtomicU64,
}

pub(super) struct OldAction {
    pub(super) handler: AtomicUsize,
    pub(super) flags: AtomicU32,
}

impl OldAction {
    const fn new() -> Self {
        Self {
            handler: AtomicUsize::new(0),
            flags: AtomicU32::new(0),
        }
    }
}

#[allow(clippy::declare_interior_mutable_const)]
const ZERO_PAIR: (AtomicU64, AtomicU64) = (AtomicU64::new(0), AtomicU64::new(0));
#[allow(clippy::declare_interior_mutable_const)]
const PC_TABLE: [(AtomicU64, AtomicU64); PC_SLOTS] = [ZERO_PAIR; PC_SLOTS];
#[allow(clippy::declare_interior_mutable_const)]
const ZERO3: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

pub(super) static SHARED: Shared = Shared {
    base: AtomicU64::new(0),
    len: AtomicU64::new(0),
    page: AtomicU64::new(0),
    table: AtomicPtr::new(std::ptr::null_mut()),
    npages: AtomicUsize::new(0),
    members: AtomicU64::new(0),
    completed: AtomicU64::new(0),
    log: [const { AtomicU32::new(0) }; LOG_CAP],
    log_n: AtomicUsize::new(0),
    internal: [const { AtomicUsize::new(0) }; INTERNAL],
    old: [OldAction::new(), OldAction::new()],
    faults: [ZERO3; 5],
    handler_ns: AtomicU64::new(0),
    host_access: [const { AtomicU64::new(0) }; 2],
    hook_dirtied: AtomicU64::new(0),
    mprotects: AtomicU64::new(0),
    mprotect_ns: AtomicU64::new(0),
    mprotect_failed: AtomicU64::new(0),
    log_overflows: AtomicU64::new(0),
    last_signal: AtomicU64::new(0),
    mirror: AtomicU64::new(0),
    wake: AtomicU32::new(0),
    flushed: AtomicU64::new(0),
    committed: AtomicU64::new(0),
    phase_writes: [const { AtomicU64::new(0) }; 3],
    p1_n: [const { AtomicU32::new(0) }; P1_RING],
    p1_log: [const { [const { AtomicU64::new(0) }; P1_CAP] }; P1_RING],
    wait_n: [const { AtomicU64::new(0) }; 2],
    wait_ns: [const { AtomicU64::new(0) }; 2],
    wait_max_ns: [const { AtomicU64::new(0) }; 2],
    wait_uncommitted: [const { AtomicU64::new(0) }; 2],
    flight_n: AtomicU32::new(0),
    flight_log: [const { AtomicU64::new(0) }; FLIGHT_CAP],
    eager: AtomicBool::new(false),
    synced: AtomicU64::new(0),
    writer_waits: AtomicU64::new(0),
    writer_wait_ns: AtomicU64::new(0),
    internal_opens: AtomicU64::new(0),
    wait_timeouts: AtomicU64::new(0),
    member_bits: AtomicPtr::new(std::ptr::null_mut()),
    member_gen: AtomicU64::new(0),
    pcs: [PC_TABLE; 5],
    pc_overflow: AtomicU64::new(0),
    info: AtomicPtr::new(std::ptr::null_mut()),
    alias: AtomicU64::new(0),
    emulate: AtomicBool::new(true),
    emu: [const { AtomicU64::new(0) }; Form::COUNT],
    emu_ns: AtomicU64::new(0),
    fallback: [const { AtomicU64::new(0) }; Reason::COUNT],
    emu_staged: AtomicU64::new(0),
    eager_on: AtomicBool::new(false),
    hot_on: AtomicBool::new(false),
    epoch: AtomicU32::new(0),
    eager_pages: AtomicU64::new(0),
    eager_ns: AtomicU64::new(0),
    eager_runs: AtomicU64::new(0),
    readers_log: [const { AtomicU32::new(0) }; READERS_CAP],
    readers_n: AtomicUsize::new(0),
    wait_end: [const { AtomicU64::new(0) }; 2],
    wait_end_ns: [const { AtomicU64::new(0) }; 2],
    early_calls: AtomicU64::new(0),
    early_pages: AtomicU64::new(0),
    early_syncs: AtomicU64::new(0),
    late: AtomicBool::new(false),
    read_open_on: AtomicBool::new(false),
    late_reads: AtomicU64::new(0),
    late_writes: AtomicU64::new(0),
    late_age_sum: AtomicU64::new(0),
    late_age: [const { AtomicU64::new(0) }; 4],
    late_age_max: AtomicU64::new(0),
    late_owned: AtomicU64::new(0),
    late_kept: AtomicU64::new(0),
    late_copied: AtomicU64::new(0),
    late_copied_blocks: AtomicU64::new(0),
    late_flush_copies: AtomicU64::new(0),
    late_exact: AtomicU64::new(0),
    late_frame_won: AtomicU64::new(0),
    late_carried: AtomicU64::new(0),
    late_carry_lost: AtomicU64::new(0),
    settled: AtomicU64::new(0),
    snap_resets: [const { AtomicU64::new(0) }; SnapWhy::COUNT],
    snap_misses: [const { AtomicU64::new(0) }; SnapWhy::COUNT],
    read_open: AtomicU64::new(0),
    read_revoked: AtomicU64::new(0),
    settle_late: AtomicU64::new(0),
    settle_waits: AtomicU64::new(0),
};
