//! Dirty tracking of guest memory pages (`NXKIT_PRIVATE=track`, see "Private GPU memory, step 1: tracking" in
//! `docs/nxkit-gpu.md`).
//!
//! The host asks which guest pages the game's CPU writes, and which pages it touches while a frame that may write them
//! runs on the GPU. The page table [`Pages`] holds one atomic word per guest page of the host page size (16 KiB):
//!
//! | state | protection | meaning |
//! |---|---|---|
//! | free | as the guest set it | not tracked |
//! | clean | read only | tracked; the CPU has not written it since the last flush |
//! | dirty | read and write | the CPU wrote it since the last flush |
//! | gpu(frame) | none | a frame that may write the page is in flight, the number is the frame |
//!
//! A write to a clean page faults; the handler marks the page dirty, logs it, opens it and returns, so the store
//! runs again. A flush ([`Pages::flush`]) takes the log, protects the dirty pages read only again, and protects the
//! pages that the new frame may write against all access. A page that the CPU touches while it is protected that way
//! is counted as an access in flight and opened (read only for a read, read and write for a write). Guest memory stays
//! the truth in this mode: nothing is copied, nothing waits. The completion of the frame ([`Pages::complete`]) puts the
//! pages that are still protected read only.
//!
//! The fault path is a signal, `SIGBUS` or `SIGSEGV` (the host installs no Mach exception port). The handler runs
//! before the crash report of the host: it takes a fault on a tracked page and hands every other fault to the handler
//! that was installed before it. It reads and writes atomics, calls `mprotect` and reads the clock; it takes no lock
//! and allocates nothing. Everything outside the handler that changes the table or the protection of a run holds one
//! mutex, which no handler takes.
//!
//! A system call cannot be held back by a fault: it returns `EFAULT`. The paths of the host that let a system call
//! read or write guest memory, or change the permission of guest pages, call `before_host_access` through the hook
//! of `nxkit_abi::guest`, which opens the tracked pages in the range first.
//!
//! Limits: the frame numbers are kept in 29 bits. The kind of access comes from the write bit of the exception
//! syndrome; an atomic read-modify-write that faults on a protected page counts as a read first and as a write at its
//! second fault. A thread that does not know the host marks no pages as internal, so its accesses count as the game's.
//!
//! Step 4 (mirrored mode): a page tagged with a frame knows the 64-byte blocks that the frames since its last copy from
//! the mirror may have written (the stale blocks). A copy from the mirror moves those blocks only. A CPU access to a
//! page of a frame in flight that lies outside the stale blocks is done by the handler itself through a second mapping
//! of guest memory ([`Pages::make_alias`], [`a64`]), without waiting and without opening the page; a store of that
//! kind is logged, and the next flush stages the page without its stale blocks.

pub(crate) mod a64;

pub use a64::{Form, Reason};

use std::collections::{BTreeMap, VecDeque};
use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, Once, PoisonError};

const PROT_NONE: c_int = 0;
const PROT_READ: c_int = 1;
const PROT_RW: c_int = 3;

const SIGBUS: c_int = 10;
const SIGSEGV: c_int = 11;
const SA_ONSTACK: c_int = 0x1;
const SA_SIGINFO: c_int = 0x40;
const CLOCK_UPTIME_RAW: u32 = 8;

/// Page states, in the low three bits of a table word.
const FREE: u32 = 0;
const CLEAN: u32 = 1;
const DIRTY: u32 = 2;
const GPU: u32 = 3;
/// A thread is copying the page from the mirror into guest memory (mirrored mode).
const SYNC: u32 = 4;
/// Like `GPU`, but a host thread opened the page for reading while its frame was in flight (mirrored mode); the
/// completion of the frame protects it again.
const OPEN: u32 = 5;
const STATE_MASK: u32 = 7;
/// Set on a `GPU` word whose guest bytes are current: the CPU wrote the page and the flush staged it, so guest memory
/// holds what the frame starts from and the mirror gets it at the start of the frame (mirrored mode).
const FRESH: u32 = 1 << 3;
/// The frame of a `GPU` word is in the bits above the state and the fresh bit: 28 bits.
const FRAME_SHIFT: u32 = 4;
const FRAME_MASK: u64 = (1 << 28) - 1;

/// The pages the log of dirty pages holds. A frame that dirties more makes the flush scan the table.
const LOG_CAP: usize = 1 << 16;
/// The slots of the table of internal threads.
const INTERNAL: usize = 32;
/// The pages the log of readers holds between two calls of [`Pages::readers`].
const READERS_CAP: usize = 1024;
/// How many frames a page stays a CPU-read GPU result after a game thread last waited for its writer.
const LEARN_FRAMES: u64 = 240;
/// The frames whose first writes between flush and commit are logged (a ring by frame number).
const P1_RING: usize = 8;
/// The pages logged for one frame.
const P1_CAP: usize = 2048;
/// The accesses to pages in flight that the log of [`Pages::take_in_flight`] holds.
const FLIGHT_CAP: usize = 1024;
/// The slots of each table of program counters.
const PC_SLOTS: usize = 512;
/// How far a program counter probes before it is counted as overflow.
const PC_PROBE: usize = 16;
/// The frames whose runs wait for their completion. A frame that never completes (an encode that failed) is released
/// when this many newer ones are waiting.
const MAX_FRAMES: usize = 24;
/// How long a thread waits for the writer of a page before it copies the page anyway.
const TIMEOUT_NS: u64 = 4_000_000_000;
/// One sleep on the wake word.
const SLICE_NS: u64 = 20_000_000;
const OS_CLOCK_MACH_ABSOLUTE_TIME: u32 = 32;

/// The bytes per block of the stale mask: a page of 16 KiB has 256 blocks, four 64-bit words.
const BLOCK: u64 = 64;
/// The blocks of a page as bits.
type Mask = [u64; 4];
/// The program counter table of the accesses that were not emulated.
const PC_FALLBACK: usize = 4;

/// What the handler reads for a page besides its table word (step 4).
struct PageInfo {
    /// The blocks that the frames since the last copy from the mirror may have written: guest memory is stale there.
    stale: [AtomicU64; 4],
    /// Bit 0: the handler emulated a store to the page since the last flush took it.
    emu: AtomicU32,
    /// The epoch plus one in which a game thread last faulted on the page for a copy from the mirror, 0 for never: the
    /// completion of a frame copies the page back on its own while the epoch is no later than this (step 5).
    eager: AtomicU32,
    /// The newest frame (28 bits) whose writer is done with this page although the frame is not complete: its last pass
    /// that writes the page ran (step 6). Zero for none.
    ready: AtomicU32,
    /// The frame (28 bits) of the newest wait of a game thread for this page's writer, 0 for never (step 6).
    waited: AtomicU32,
    /// Results one frame late: the frame (28 bits) whose completion the stale blocks of guest memory hold, 0 when guest
    /// memory is not known to hold a completed frame's bytes there. A game thread may read (and write) the stale blocks of
    /// a page in flight through the alias while it is not zero.
    snap: AtomicU32,
    /// Why `snap` was last set to zero ([`SnapWhy`] as a number), 0 for never set.
    snap_why: AtomicU32,
    /// Results one frame late: the blocks the CPU wrote while a frame in flight wrote them, not staged yet. The copy from
    /// the mirror leaves them alone and the next flush stages them.
    own: [AtomicU64; 4],
    /// The newest frame that was flushed when the CPU wrote a block of `own`.
    own_frame: AtomicU32,
    /// The blocks of `own` that a flush staged: the copy-back of a frame up to `hold_frame` leaves them alone, because the
    /// CPU wrote them after that frame was flushed.
    hold: [AtomicU64; 4],
    hold_frame: AtomicU32,
}

/// Why the guest bytes of a page stopped being a completed frame's (`PageInfo::snap` went to zero), and with it the
/// right of game threads to read the page late. The last reason is kept per page, so a late access that found no snapshot
/// names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum SnapWhy {
    /// No copy has set a snapshot yet (the first read of the page waits and teaches it).
    Never = 0,
    /// The page was released or the tracker attached again.
    Released,
    /// A thread copied or had the page open at the completion of a frame.
    Busy,
    /// The blocks the frame wrote were not known at its completion.
    Unknown,
    /// No game thread read the page lately: the completion left it alone.
    Idle,
    /// The flush found a page nobody read lately.
    FlushIdle,
    /// The poison pattern replaced the bytes.
    Poison,
}

impl SnapWhy {
    /// The number of reasons.
    pub const COUNT: usize = 7;
    /// All reasons, in order.
    pub const ALL: [SnapWhy; Self::COUNT] = [
        SnapWhy::Never,
        SnapWhy::Released,
        SnapWhy::Busy,
        SnapWhy::Unknown,
        SnapWhy::Idle,
        SnapWhy::FlushIdle,
        SnapWhy::Poison,
    ];

    /// A short name for the diagnostics.
    pub fn name(self) -> &'static str {
        match self {
            SnapWhy::Never => "never set",
            SnapWhy::Released => "released",
            SnapWhy::Busy => "busy at the completion",
            SnapWhy::Unknown => "blocks unknown",
            SnapWhy::Idle => "nobody read it lately (completion)",
            SnapWhy::FlushIdle => "nobody read it lately (flush)",
            SnapWhy::Poison => "poison",
        }
    }
}

impl PageInfo {
    /// Drops the snapshot of the page for `why` and counts the reason. A page that had none is left alone.
    fn snap_reset(&self, why: SnapWhy) {
        if self.snap.swap(0, Ordering::AcqRel) != 0 {
            self.snap_why.store(why as u32, Ordering::Relaxed);
            SHARED.snap_resets[why as usize].fetch_add(1, Ordering::Relaxed);
        }
    }

    fn stale(&self) -> Mask {
        [
            self.stale[0].load(Ordering::Acquire),
            self.stale[1].load(Ordering::Acquire),
            self.stale[2].load(Ordering::Acquire),
            self.stale[3].load(Ordering::Acquire),
        ]
    }

    fn or_stale(&self, m: &Mask) {
        for (a, b) in self.stale.iter().zip(m) {
            // A page that frames write over and over has the bits already: no read-modify-write then.
            if *b != 0 && a.load(Ordering::Relaxed) & *b != *b {
                a.fetch_or(*b, Ordering::AcqRel);
            }
        }
    }

    fn clear(&self) {
        for a in &self.stale {
            a.store(0, Ordering::Release);
        }
        self.emu.store(0, Ordering::Release);
        self.eager.store(0, Ordering::Release);
        self.ready.store(0, Ordering::Release);
        self.waited.store(0, Ordering::Release);
        self.snap.store(0, Ordering::Release);
        self.snap_why.store(SnapWhy::Released as u32, Ordering::Relaxed);
        self.hold_frame.store(0, Ordering::Release);
        self.own_frame.store(0, Ordering::Release);
        for a in self.own.iter().chain(&self.hold) {
            a.store(0, Ordering::Release);
        }
    }

    fn load_mask(a: &[AtomicU64; 4]) -> Mask {
        [
            a[0].load(Ordering::Acquire),
            a[1].load(Ordering::Acquire),
            a[2].load(Ordering::Acquire),
            a[3].load(Ordering::Acquire),
        ]
    }

    /// The blocks that a copy from the mirror for frame `frame` must leave alone: the ones the CPU wrote and no flush
    /// staged yet, and the ones a flush staged for a write after a frame that is not older than `frame`.
    fn keep(&self, frame: u32) -> Mask {
        let own = Self::load_mask(&self.own);
        if frame > self.hold_frame.load(Ordering::Acquire) {
            return own;
        }
        let hold = Self::load_mask(&self.hold);
        std::array::from_fn(|i| own[i] | hold[i])
    }

    /// The mask for [`Stager::stage_masked`]: the stale blocks that the CPU did not write. The blocks the CPU wrote move to
    /// `hold` (they go into the mirror with this staging) and protect the copy-back of the frames flushed before the write. A
    /// `hold` that no frame in flight needs any more is dropped first.
    fn take_skip(&self) -> Mask {
        let stale = self.stale();
        if self.own.iter().all(|a| a.load(Ordering::Relaxed) == 0) {
            return stale;
        }
        let hf = self.hold_frame.load(Ordering::Acquire);
        if u64::from(hf) <= SHARED.completed.load(Ordering::Acquire) & FRAME_MASK {
            for a in &self.hold {
                a.store(0, Ordering::Release);
            }
        }
        let mut skip = stale;
        for (i, a) in self.own.iter().enumerate() {
            let taken = a.swap(0, Ordering::AcqRel);
            self.hold[i].fetch_or(taken, Ordering::AcqRel);
            skip[i] &= !taken;
        }
        self.hold_frame.fetch_max(self.own_frame.load(Ordering::Acquire), Ordering::AcqRel);
        skip
    }
}

/// The mask for the staging of page `pg`.
fn stage_skip(pg: usize) -> Mask {
    info(pg).map_or([0; 4], PageInfo::take_skip)
}

/// What kind of fault, for the counters and the program counter tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultKind {
    /// A write to a clean page (the CPU dirties it).
    CleanWrite = 0,
    /// A read of a page that a frame in flight may write.
    ReadInFlight = 1,
    /// A write to a page that a frame in flight may write.
    WriteInFlight = 2,
    /// Anything else: a race with a flush or a completion, an access to a page whose frame is complete.
    Other = 3,
    /// An access to a page whose frame is complete: the page was copied from the mirror into guest memory (mirrored
    /// mode).
    Sync = 4,
}

/// Who faulted: guest code, host code on a game thread, or a thread the host marked internal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultOrigin {
    /// The program counter is in guest memory.
    Guest = 0,
    /// The program counter is in the host (a service, the walker of the recording, a driver call).
    Host = 1,
    /// A thread marked with [`mark_internal_thread`] (the encoder, the writer of the counter reports).
    Internal = 2,
}

/// `struct sigaction` of `<sys/signal.h>`.
#[repr(C)]
#[derive(Clone, Copy)]
struct SigAction {
    handler: usize,
    mask: u32,
    flags: c_int,
}

/// `siginfo_t`, up to `si_addr`.
#[repr(C)]
struct SigInfo {
    signo: c_int,
    errno: c_int,
    code: c_int,
    pid: c_int,
    uid: u32,
    status: c_int,
    addr: usize,
}

type Handler = extern "C" fn(c_int, *mut SigInfo, *mut c_void);

const MAP_PRIVATE: c_int = 0x2;
const MAP_ANON: c_int = 0x1000;
const MAP_NORESERVE: c_int = 0x40;
const VM_FLAGS_FIXED: c_int = 0;
const VM_FLAGS_OVERWRITE: c_int = 0x4000;
const VM_INHERIT_NONE: c_int = 2;

unsafe extern "C" {
    static mach_task_self_: u32;
    fn mach_vm_remap(
        target_task: u32,
        target_address: *mut u64,
        size: u64,
        mask: u64,
        flags: c_int,
        src_task: u32,
        src_address: u64,
        copy: c_int,
        cur_protection: *mut c_int,
        max_protection: *mut c_int,
        inheritance: c_int,
    ) -> c_int;
    fn mach_vm_protect(task: u32, address: u64, size: u64, set_maximum: c_int, prot: c_int) -> c_int;
    fn mmap(addr: *mut c_void, len: usize, prot: c_int, flags: c_int, fd: c_int, off: i64) -> *mut c_void;
    fn munmap(addr: *mut c_void, len: usize) -> c_int;
    fn mprotect(addr: *mut c_void, len: usize, prot: c_int) -> c_int;
    fn getpagesize() -> c_int;
    fn sigaction(sig: c_int, act: *const SigAction, old: *mut SigAction) -> c_int;
    fn pthread_self() -> usize;
    fn clock_gettime_nsec_np(clock: u32) -> u64;
    fn os_sync_wait_on_address_with_timeout(
        addr: *mut c_void,
        value: u64,
        size: usize,
        flags: u32,
        clockid: u32,
        timeout_ns: u64,
    ) -> c_int;
    fn os_sync_wake_by_address_all(addr: *mut c_void, size: usize, flags: u32) -> c_int;
    fn dladdr(addr: *const c_void, info: *mut DlInfo) -> c_int;
}

/// `Dl_info`.
#[repr(C)]
struct DlInfo {
    fname: *const std::ffi::c_char,
    fbase: usize,
    sname: *const std::ffi::c_char,
    saddr: usize,
}

fn now_ns() -> u64 {
    // SAFETY: the call has no preconditions and is safe in a signal handler.
    unsafe { clock_gettime_nsec_np(CLOCK_UPTIME_RAW) }
}

fn pack(state: u32, frame: u64) -> u32 {
    state | (((frame & FRAME_MASK) as u32) << FRAME_SHIFT)
}

/// What the signal handler reads, and the counters.
struct Shared {
    base: AtomicU64,
    len: AtomicU64,
    page: AtomicU64,
    /// The table: `npages` words, zero for free.
    table: AtomicPtr<AtomicU32>,
    npages: AtomicUsize,
    /// The pages that are not free.
    members: AtomicU64,
    /// The newest frame whose completion was reported.
    completed: AtomicU64,
    /// The pages that went dirty since the last flush, as page number plus one; a slot is zero until its writer filled
    /// it.
    log: [AtomicU32; LOG_CAP],
    log_n: AtomicUsize,
    internal: [AtomicUsize; INTERNAL],
    old: [OldAction; 2],
    /// Faults by `[kind][origin]`.
    faults: [[AtomicU64; 3]; 5],
    /// Time in the handler, for tracked pages.
    handler_ns: AtomicU64,
    /// Accesses by system calls (the hook) to pages of a frame in flight: read, write.
    host_access: [AtomicU64; 2],
    /// Pages dirtied by the hook rather than by a fault.
    hook_dirtied: AtomicU64,
    /// Calls of `mprotect`, and the time of those outside the handler.
    mprotects: AtomicU64,
    mprotect_ns: AtomicU64,
    mprotect_failed: AtomicU64,
    /// Pages the log could not hold.
    log_overflows: AtomicU64,
    /// The last signal of a tracked fault.
    last_signal: AtomicU64,
    /// The host address of the mirror of guest memory (byte `o` of guest memory is at `mirror + o`), 0 when there is no
    /// mirror. With a mirror, a page tagged with a frame holds stale guest bytes and the handler copies it.
    mirror: AtomicU64,
    /// Bumped, and woken on, at each completion and each finished copy.
    wake: AtomicU32,
    /// The newest frame whose flush ran, and the newest whose commit was reported. Where the newest frame is follows from
    /// them and `completed`: 1 flushed and not committed, 2 committed and running, 0 done. Writes that dirty a page are
    /// counted by it.
    flushed: AtomicU64,
    committed: AtomicU64,
    phase_writes: [AtomicU64; 3],
    /// The addresses of the first writes to member pages between the flush of a frame and its commit, by frame number
    /// modulo [`P1_RING`], as the byte offset in guest memory plus one, and how many writes the frame logged (the log
    /// holds [`P1_CAP`]).
    p1_n: [AtomicU32; P1_RING],
    p1_log: [[AtomicU64; P1_CAP]; P1_RING],
    /// Waits of a thread for the writer of a page, by access (0 read, 1 write): count, time, the longest, and how many
    /// began before the frame was committed (the wait then includes the encode).
    wait_n: [AtomicU64; 2],
    wait_ns: [AtomicU64; 2],
    wait_max_ns: [AtomicU64; 2],
    wait_uncommitted: [AtomicU64; 2],
    /// The first access of a game thread to a page of a frame in flight, as `frame << 36 | write << 35 | page + 1`, and
    /// how many there were since the last [`Pages::take_in_flight`].
    flight_n: AtomicU32,
    flight_log: [AtomicU64; FLIGHT_CAP],
    /// With a mirror: copy every page of a completed frame back into guest memory at once (a bisecting aid).
    eager: AtomicBool,
    /// Pages copied from the mirror into guest memory.
    synced: AtomicU64,
    /// Waits for a writer that was still running, and their time.
    writer_waits: AtomicU64,
    writer_wait_ns: AtomicU64,
    /// Accesses of internal threads to pages in flight, served without waiting.
    internal_opens: AtomicU64,
    /// Waits that ended at the timeout.
    wait_timeouts: AtomicU64,
    /// The member bitmap (one bit per page, set while the page is not free) and its generation.
    member_bits: AtomicPtr<AtomicU64>,
    member_gen: AtomicU64,
    /// Program counters by [`FaultKind`] 0 to 2: the counter and how often it faulted.
    pcs: [[(AtomicU64, AtomicU64); PC_SLOTS]; 5],
    pc_overflow: AtomicU64,
    /// The per-page stale blocks and flags: `npages` entries.
    info: AtomicPtr<PageInfo>,
    /// The address of the alias of the byte at `base` (a second mapping of guest memory with read and write access), 0
    /// when there is none.
    alias: AtomicU64,
    /// The handler emulates accesses outside the stale blocks.
    emulate: AtomicBool,
    /// Emulated accesses by form, the time they took in the handler, and the accesses that fell back by reason.
    emu: [AtomicU64; Form::COUNT],
    emu_ns: AtomicU64,
    fallback: [AtomicU64; Reason::COUNT],
    /// Pages the flush staged because the handler stored to them.
    emu_staged: AtomicU64,
    /// Step 5: the completion of a frame copies back, on its own thread, the pages that game threads faulted on for a
    /// copy lately; pages that the CPU writes in many flushes in a row stay writable and are diffed at each flush.
    eager_on: AtomicBool,
    hot_on: AtomicBool,
    /// The epoch (`frame / EPOCH_FRAMES`) of the newest flush.
    epoch: AtomicU32,
    /// Pages the eager copy moved, the time it took, and the pages it found changed by someone else.
    eager_pages: AtomicU64,
    eager_ns: AtomicU64,
    eager_runs: AtomicU64,
    /// Step 6: the pages a game thread waited for (page plus one), until [`Pages::readers`] takes them.
    readers_log: [AtomicU32; READERS_CAP],
    readers_n: AtomicUsize,
    /// Waits by what ended them (0 the pass signal of the page, the frame still running; 1 the completion of the whole
    /// frame) and their time.
    wait_end: [AtomicU64; 2],
    wait_end_ns: [AtomicU64; 2],
    /// Calls of [`Pages::release_early`], the pages they released, and the faults that found a page released while its frame
    /// ran.
    early_calls: AtomicU64,
    early_pages: AtomicU64,
    early_syncs: AtomicU64,
    /// Results one frame late: the handler serves the CPU's accesses to the blocks a frame in flight writes from guest
    /// memory, which holds the last completed frame's bytes, without waiting.
    late: AtomicBool,
    /// Results one frame late: the flush leaves a page readable when its guest bytes are a completed frame's.
    read_open_on: AtomicBool,
    /// Late reads and writes, the age of the bytes they met (frames: the frame in flight minus the frame the bytes are
    /// from) as a sum, a histogram (1, 2, 3, 4 or more) and the largest since the last [`Pages::take_late_age_max`], the
    /// blocks the CPU took from the frame (written while it ran), the blocks a copy from the mirror left alone for that
    /// reason, the pages and blocks the completion of a frame copied for the next ones, and the pages the flush copied.
    late_reads: AtomicU64,
    late_writes: AtomicU64,
    late_age_sum: AtomicU64,
    late_age: [AtomicU64; 4],
    late_age_max: AtomicU64,
    late_owned: AtomicU64,
    late_kept: AtomicU64,
    late_copied: AtomicU64,
    late_copied_blocks: AtomicU64,
    late_flush_copies: AtomicU64,
    /// Snapshots dropped, by [`SnapWhy`], and late accesses that found no snapshot, by the reason the page lost it last.
    snap_resets: [AtomicU64; SnapWhy::COUNT],
    snap_misses: [AtomicU64; SnapWhy::COUNT],
    /// Pages that the flush left readable while a frame in flight writes them (results one frame late: guest memory holds
    /// the last completed frame's bytes there), and the pages that lost this at a completion.
    read_open: AtomicU64,
    read_revoked: AtomicU64,
    /// System calls that read a page in flight outside its stale blocks without a wait, and the ones that waited for a writer.
    settle_late: AtomicU64,
    settle_waits: AtomicU64,
}

struct OldAction {
    handler: AtomicUsize,
    flags: AtomicU32,
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

static SHARED: Shared = Shared {
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
    snap_resets: [const { AtomicU64::new(0) }; SnapWhy::COUNT],
    snap_misses: [const { AtomicU64::new(0) }; SnapWhy::COUNT],
    read_open: AtomicU64::new(0),
    read_revoked: AtomicU64::new(0),
    settle_late: AtomicU64::new(0),
    settle_waits: AtomicU64::new(0),
};

/// Flushes per epoch of the eager copy: a page that game threads stop faulting on loses its place after two epochs.
const EPOCH_FRAMES: u64 = 256;
/// Consecutive flushes in which the CPU dirtied a page before it stays writable.
const HOT_AFTER: u8 = 3;
/// Consecutive flushes in which a writable page did not change before it is protected again.
const HOT_QUIET: u8 = 8;

/// What only the holders of the mutex touch.
struct Ctrl {
    /// The runs of pages (first, end) that each frame in flight protected, oldest first.
    frames: VecDeque<(u64, Vec<(u32, u32)>)>,
    /// The blocks each of those frames may write, by run and page (results one frame late, mirrored mode).
    masks: VecDeque<FrameMasks>,
    /// The member bitmap at a generation, for [`Pages::member_snapshot`].
    snap: Option<(u64, std::sync::Arc<Vec<u64>>)>,
    /// Step 5: for each page, in how many flushes in a row the CPU dirtied it (capped), and the pages dirtied in the last
    /// flush.
    streak: Vec<u8>,
    prev_dirty: Vec<u32>,
    /// The pages that stay writable.
    hot: BTreeMap<u32, Hot>,
}

/// The pages that game threads waited for lately: CPU-read GPU results (step 6). Its own lock: the encoder takes it, not the
/// flush.
struct Learned {
    pages: std::collections::BTreeSet<u32>,
    changes: u64,
}

static LEARNED: Mutex<Learned> = Mutex::new(Learned {
    pages: std::collections::BTreeSet::new(),
    changes: 0,
});

/// A page that stays writable: the bytes staged for it last (empty until the first staging) and the flushes since they
/// changed.
struct Hot {
    shadow: Vec<u8>,
    quiet: u8,
}

static CTRL: Mutex<Ctrl> = Mutex::new(Ctrl {
    frames: VecDeque::new(),
    masks: VecDeque::new(),
    snap: None,
    streak: Vec::new(),
    prev_dirty: Vec::new(),
    hot: BTreeMap::new(),
});
static INSTALL: Once = Once::new();

fn ctrl() -> MutexGuard<'static, Ctrl> {
    CTRL.lock().unwrap_or_else(PoisonError::into_inner)
}

fn page() -> u64 {
    let p = SHARED.page.load(Ordering::Relaxed);
    if p != 0 {
        return p;
    }
    // SAFETY: `getpagesize` has no preconditions.
    let p = u64::try_from(unsafe { getpagesize() }).unwrap_or(0x4000);
    SHARED.page.store(p, Ordering::Relaxed);
    p
}

/// The table word of `page`, None when there is no table or the page is outside it.
fn word(page: usize) -> Option<&'static AtomicU32> {
    let p = SHARED.table.load(Ordering::Acquire);
    if p.is_null() || page >= SHARED.npages.load(Ordering::Acquire) {
        return None;
    }
    // SAFETY: the table has `npages` words that are never freed while the process runs (a new `attach` leaks the old
    // one), and `page` is inside it.
    Some(unsafe { &*p.add(page) })
}

/// The stale blocks and flags of `page`, None when there is no table or the page is outside it.
fn info(page: usize) -> Option<&'static PageInfo> {
    let p = SHARED.info.load(Ordering::Acquire);
    if p.is_null() || page >= SHARED.npages.load(Ordering::Acquire) {
        return None;
    }
    // SAFETY: the array has `npages` entries that are never freed while the process runs (a new `attach` leaks the old
    // one), zero bytes are a valid entry, and `page` is inside it.
    Some(unsafe { &*p.add(page) })
}

fn set_prot_raw(start: u64, end: u64, prot: c_int) -> bool {
    SHARED.mprotects.fetch_add(1, Ordering::Relaxed);
    // SAFETY: the run lies in guest memory that `attach` named; the run is whole host pages. `mprotect` only changes
    // the access to them.
    let ok = unsafe { mprotect(start as *mut c_void, (end - start) as usize, prot) == 0 };
    if !ok {
        SHARED.mprotect_failed.fetch_add(1, Ordering::Relaxed);
    }
    ok
}

/// Protects the pages `[first, end)` (page numbers). Outside the handler this also adds the time to the counters.
fn protect_pages(first: u32, end: u32, prot: c_int, timed: bool) -> bool {
    let base = SHARED.base.load(Ordering::Acquire);
    let p = page();
    let t0 = timed.then(now_ns);
    let ok = set_prot_raw(
        base + u64::from(first) * p,
        base + u64::from(end) * p,
        prot,
    );
    if let Some(t0) = t0 {
        SHARED
            .mprotect_ns
            .fetch_add(now_ns().saturating_sub(t0), Ordering::Relaxed);
    }
    ok
}

/// Page numbers (first, end) of the host range `[addr, addr + len)`, clipped to the table. None when empty.
fn page_span(addr: u64, len: u64) -> Option<(u32, u32)> {
    let base = SHARED.base.load(Ordering::Acquire);
    let total = SHARED.len.load(Ordering::Acquire);
    let p = page();
    let lo = addr.max(base);
    let hi = addr.saturating_add(len.max(1)).min(base.saturating_add(total));
    if lo >= hi {
        return None;
    }
    let first = (lo - base) / p;
    let end = (hi - base).div_ceil(p);
    let n = SHARED.npages.load(Ordering::Acquire) as u64;
    (first < n).then(|| (first as u32, end.min(n) as u32))
}

/// Pushes a page that went dirty into the log.
fn log_dirty(page: usize) {
    let i = SHARED.log_n.fetch_add(1, Ordering::AcqRel);
    if i < LOG_CAP {
        SHARED.log[i].store(page as u32 + 1, Ordering::Release);
    } else {
        SHARED.log_overflows.fetch_add(1, Ordering::Relaxed);
    }
}

fn is_internal() -> bool {
    // SAFETY: `pthread_self` has no preconditions.
    let me = unsafe { pthread_self() };
    SHARED
        .internal
        .iter()
        .any(|s| s.load(Ordering::Acquire) == me)
}

/// Marks the calling thread as part of the host's own way to the completion of a frame (the encoder thread, the code
/// that writes the counter reports). Its accesses to tracked pages are counted apart from the game's. The mark ends
/// when the guard drops.
pub fn mark_internal_thread() -> InternalThread {
    // SAFETY: `pthread_self` has no preconditions.
    let me = unsafe { pthread_self() };
    for (i, s) in SHARED.internal.iter().enumerate() {
        if s.compare_exchange(0, me, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            return InternalThread(Some(i));
        }
    }
    InternalThread(None)
}

/// The mark of [`mark_internal_thread`].
#[derive(Debug)]
pub struct InternalThread(Option<usize>);

impl Drop for InternalThread {
    fn drop(&mut self) {
        if let Some(i) = self.0 {
            SHARED.internal[i].store(0, Ordering::Release);
        }
    }
}

fn count_pc(kind: FaultKind, pc: u64) {
    let k = kind as usize;
    if k > 2 {
        return;
    }
    count_pc_in(k, pc);
}

/// Counts `pc` in table `k` (0 to 2 by fault kind, 3 for writes between a flush and the commit of its frame).
fn count_pc_in(k: usize, pc: u64) {
    if pc == 0 {
        return;
    }
    let table = &SHARED.pcs[k];
    for i in 0..PC_PROBE {
        let (key, n) = &table[((pc >> 2) as usize).wrapping_add(i) % PC_SLOTS];
        match key.compare_exchange(0, pc, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => {
                n.fetch_add(1, Ordering::Relaxed);
                return;
            }
            Err(v) if v == pc => {
                n.fetch_add(1, Ordering::Relaxed);
                return;
            }
            Err(_) => {}
        }
    }
    SHARED.pc_overflow.fetch_add(1, Ordering::Relaxed);
}

/// The program counter and the write bit of the exception syndrome of the faulting instruction.
fn fault_regs(uctx: *mut c_void) -> (u64, bool) {
    if uctx.is_null() {
        return (0, false);
    }
    // SAFETY: the kernel passes a valid `ucontext_t` to an SA_SIGINFO handler. Its `uc_mcontext` pointer is at byte 48;
    // the `__darwin_mcontext64` it points at starts with the exception state (`far` at 0, `esr` at 8) and holds the
    // program counter at byte 272. The fields are read, not written.
    unsafe {
        let mc = *uctx.cast::<u8>().add(48).cast::<*const u8>();
        if mc.is_null() {
            (0, false)
        } else {
            let esr = *mc.add(8).cast::<u32>();
            (*mc.add(272).cast::<u64>(), esr & (1 << 6) != 0)
        }
    }
}

/// The registers of the faulting thread in the saved state of the signal, and the address of its program counter.
///
/// # Safety
/// `uctx` must be the context that the kernel passed to an `SA_SIGINFO` handler of this thread on arm64 macOS.
unsafe fn thread_regs(uctx: *mut c_void) -> Option<(a64::Regs, *mut u64)> {
    if uctx.is_null() {
        return None;
    }
    // SAFETY: the `ucontext_t` is valid; its `uc_mcontext` pointer is at byte 48. The `__darwin_mcontext64` it points
    // at holds the exception state (16 bytes), then x0 to x30 (31 words in a row, `fp` and `lr` are x29 and x30), the
    // stack pointer at byte 264, the program counter at 272, `cpsr` and a pad word, and the SIMD registers v0 to v31 at
    // byte 288. The pointers stay valid until the handler returns.
    unsafe {
        let mc = *uctx.cast::<u8>().add(48).cast::<*mut u8>();
        if mc.is_null() {
            return None;
        }
        Some((
            a64::Regs {
                x: mc.add(16).cast(),
                sp: mc.add(264).cast(),
                v: mc.add(288).cast(),
            },
            mc.add(272).cast(),
        ))
    }
}

fn count_fallback(why: Reason, pc: u64) {
    SHARED.fallback[why as usize].fetch_add(1, Ordering::Relaxed);
    count_pc_in(PC_FALLBACK, pc);
}

/// Does the access that faulted on the page `pg` in place of the thread, when it lies outside the blocks that a frame
/// in flight may write: the access runs through the alias, the registers are filled or read, and the program counter
/// moves on. Returns false when the access must take the ordinary path (wait for the writer, copy, open). Atomics and
/// the access itself; no lock, no allocation, no system call.
fn try_emulate(pg: usize, w: &AtomicU32, pc: u64, far: u64, uctx: *mut c_void) -> bool {
    let alias = SHARED.alias.load(Ordering::Acquire);
    if alias == 0 || pc == 0 || !SHARED.emulate.load(Ordering::Relaxed) {
        return false;
    }
    let cur = w.load(Ordering::Acquire);
    if !matches!(cur & STATE_MASK, GPU | OPEN) || !page_flying(pg, cur >> FRAME_SHIFT) {
        return false;
    }
    let Some(info) = info(pg) else { return false };
    let t0 = now_ns();
    // SAFETY: a fault at `pc` means the instruction there was fetched, so its word is readable.
    let insn = unsafe { (pc as *const u32).read() };
    let op = match a64::decode_op(insn) {
        Ok(op) => op,
        Err(why) => {
            count_fallback(why, pc);
            return false;
        }
    };
    // SAFETY: the context comes from the handler of this thread.
    let Some((regs, pc_at)) = (unsafe { thread_regs(uctx) }) else {
        return false;
    };
    // SAFETY: `regs` points into the saved state of this thread.
    let plan = unsafe { a64::plan_op(&op, &regs) };
    let (base, p) = (SHARED.base.load(Ordering::Relaxed), page());
    let (first, last) = (plan.ea, plan.ea.wrapping_add(plan.len - 1));
    // The access may cross into the next page (or come from the one before): both pages are looked at, the faulting one
    // `pg` (in flight, known) and its neighbour, whose protection did not matter for the fault.
    let page_of = |a: u64| (a >= base).then(|| ((a - base) / p) as usize);
    let (Some(pa), Some(pb)) = (page_of(first), page_of(last)) else {
        count_fallback(Reason::Mismatch, pc);
        return false;
    };
    if far < first || far > last || pg < pa || pg > pb || pb > pa + 1 {
        count_fallback(Reason::Mismatch, pc);
        return false;
    }
    // Results one frame late: guest memory holds the bytes of the last completed frame in the stale blocks, and the access
    // goes on through the alias without a wait. A store marks the blocks as the CPU's before it is done, so the copy of
    // the frame's bytes that may run at the same time leaves them alone, and the next flush stages them.
    let mut late = false;
    for q in pa..=pb {
        let (qlo, qhi) = (
            if q == pa { first } else { base + q as u64 * p },
            if q == pb { last } else { base + (q as u64 + 1) * p - 1 },
        );
        let (info_q, cur_q) = if q == pg {
            (info, cur)
        } else {
            let Some(wq) = word(q) else {
                count_fallback(Reason::Mismatch, pc);
                return false;
            };
            let cur_q = wq.load(Ordering::Acquire);
            match cur_q & STATE_MASK {
                // Not tracked: nothing to look at.
                FREE => continue,
                CLEAN | DIRTY | GPU | OPEN => {}
                _ => {
                    count_fallback(Reason::Mismatch, pc);
                    return false;
                }
            }
            let Some(iq) = self::info(q) else {
                count_fallback(Reason::Mismatch, pc);
                return false;
            };
            (iq, cur_q)
        };
        let stale = info_q.stale();
        let (lo, hi) = (((qlo - base) % p) / BLOCK, ((qhi - base) % p) / BLOCK);
        let is_stale = |b: u64| stale[(b / 64) as usize] >> (b % 64) & 1 != 0;
        if !(lo..=hi).any(is_stale) {
            continue;
        }
        let snap = info_q.snap.load(Ordering::Acquire);
        let flying = q == pg || (matches!(cur_q & STATE_MASK, GPU | OPEN) && page_flying(q, cur_q >> FRAME_SHIFT));
        if snap == 0 || !SHARED.late.load(Ordering::Relaxed) || !flying {
            if snap == 0 && flying {
                let why = info_q.snap_why.load(Ordering::Relaxed) as usize;
                SHARED.snap_misses[why.min(SnapWhy::COUNT - 1)].fetch_add(1, Ordering::Relaxed);
            }
            count_fallback(Reason::Overlap, pc);
            return false;
        }
        late = true;
        if op.writes() {
            let mut owned = 0u64;
            for b in (lo..=hi).filter(|&b| is_stale(b)) {
                info_q.own[(b / 64) as usize].fetch_or(1 << (b % 64), Ordering::AcqRel);
                owned += 1;
            }
            info_q.own_frame.fetch_max((SHARED.flushed.load(Ordering::Acquire) & FRAME_MASK) as u32, Ordering::AcqRel);
            SHARED.late_owned.fetch_add(owned, Ordering::Relaxed);
        }
        let age = (cur_q >> FRAME_SHIFT).saturating_sub(snap).max(1);
        SHARED.late_age_sum.fetch_add(u64::from(age), Ordering::Relaxed);
        SHARED.late_age[(age as usize - 1).min(3)].fetch_add(1, Ordering::Relaxed);
        SHARED.late_age_max.fetch_max(u64::from(age), Ordering::Relaxed);
        // The page is a CPU-read GPU result: the completion of the frames keeps copying it.
        info_q.eager.store(SHARED.epoch.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
    }
    // SAFETY: the alias maps the same pages as guest memory with read and write access, and `plan` lies inside the
    // pages `pa` and `pb`, which lie inside guest memory. The registers are the thread's own.
    unsafe {
        if !a64::perform_op(&op, &regs, &plan, (alias + (first - base)) as *mut u8) {
            // An atomic on an address that is not aligned: the hardware would fault; the ordinary path decides.
            count_fallback(Reason::Mismatch, pc);
            return false;
        }
        pc_at.write(pc + 4);
    }
    if op.writes() {
        for q in pa..=pb {
            let at = if q == pa { first } else { base + q as u64 * p };
            let Some(info_q) = (if q == pg { Some(info) } else { self::info(q) }) else {
                continue;
            };
            // A dirty page is staged by the flush anyway; a page that is not tracked is not staged at all.
            let state = if q == pg {
                cur & STATE_MASK
            } else {
                word(q).map_or(FREE, |w| w.load(Ordering::Acquire) & STATE_MASK)
            };
            if matches!(state, FREE | DIRTY) {
                continue;
            }
            if info_q.emu.fetch_or(1, Ordering::AcqRel) & 1 == 0 {
                // The first store since the flush took the page: the next flush stages it, and it counts as a first write
                // by the phase of the newest frame, as a write that dirties a page does.
                log_dirty(q);
                let (ph, newest) = phase_now();
                SHARED.phase_writes[ph].fetch_add(1, Ordering::Relaxed);
                if ph == 1 {
                    let r = newest as usize % P1_RING;
                    let n = SHARED.p1_n[r].fetch_add(1, Ordering::AcqRel) as usize;
                    if n < P1_CAP {
                        SHARED.p1_log[r][n].store(at.wrapping_sub(base) + 1, Ordering::Release);
                    }
                    count_pc_in(3, pc);
                }
            }
        }
    }
    if late {
        let c = if op.writes() { &SHARED.late_writes } else { &SHARED.late_reads };
        c.fetch_add(1, Ordering::Relaxed);
    }
    SHARED.emu[op.form() as usize].fetch_add(1, Ordering::Relaxed);
    SHARED.emu_ns.fetch_add(now_ns().saturating_sub(t0), Ordering::Relaxed);
    true
}

/// True when `frame` (28 bits) is a frame whose completion has not been reported.
fn in_flight(frame: u32) -> bool {
    u64::from(frame) > SHARED.completed.load(Ordering::Acquire) & FRAME_MASK
}

/// True when the writer of page `pg` in `frame` still runs: the frame is not complete and the last pass that writes the page
/// has not reported (step 6).
fn page_flying(pg: usize, frame: u32) -> bool {
    in_flight(frame) && info(pg).is_none_or(|i| i.ready.load(Ordering::Acquire) < frame)
}

/// A game thread is about to wait for the writer of `pg` in `frame`: the page becomes a CPU-read GPU result for
/// [`Pages::readers`] (atomics only).
fn learn_reader(pg: usize, frame: u32) {
    let Some(i) = info(pg) else { return };
    let prev = i.waited.swap(frame, Ordering::Relaxed);
    if prev == 0 || u64::from(frame.saturating_sub(prev)) >= LEARN_FRAMES / 4 {
        let n = SHARED.readers_n.fetch_add(1, Ordering::AcqRel);
        if n < READERS_CAP {
            SHARED.readers_log[n].store(pg as u32 + 1, Ordering::Release);
        }
    }
}

/// Sets or clears the member bit of `page`. Not for the signal handler.
fn member_bit(page: usize, set: bool) {
    let p = SHARED.member_bits.load(Ordering::Acquire);
    if p.is_null() || page >= SHARED.npages.load(Ordering::Acquire) {
        return;
    }
    // SAFETY: the bitmap has `npages.div_ceil(64)` words that are never freed while the process runs (a new `attach`
    // leaks the old one), and `page` is inside the table.
    let w = unsafe { &*p.add(page / 64) };
    let bit = 1u64 << (page % 64);
    if set {
        w.fetch_or(bit, Ordering::AcqRel);
    } else {
        w.fetch_and(!bit, Ordering::AcqRel);
    }
    SHARED.member_gen.fetch_add(1, Ordering::AcqRel);
}

/// Sleeps until the wake word changes from `seen` or the slice ends.
fn sleep_on_wake(seen: u32) {
    // SAFETY: the address of a static atomic of 4 bytes; the kernel reads 4 bytes. Safe in a signal handler.
    unsafe {
        os_sync_wait_on_address_with_timeout(
            (&raw const SHARED.wake).cast_mut().cast(),
            u64::from(seen),
            4,
            0,
            OS_CLOCK_MACH_ABSOLUTE_TIME,
            SLICE_NS,
        );
    }
}

fn wake_all() {
    SHARED.wake.fetch_add(1, Ordering::AcqRel);
    // SAFETY: the address of a static atomic of 4 bytes.
    unsafe {
        os_sync_wake_by_address_all((&raw const SHARED.wake).cast_mut().cast(), 4, 0);
    }
}

/// Where the newest frame is, and its number: 1 flushed and not committed, 2 committed and running, 0 done.
fn phase_now() -> (usize, u64) {
    let f = SHARED.flushed.load(Ordering::Acquire);
    let c = SHARED.committed.load(Ordering::Acquire);
    if f > c {
        (1, f)
    } else if c > SHARED.completed.load(Ordering::Acquire) {
        (2, c)
    } else {
        (0, c)
    }
}

/// Waits until the writer of page `pg` in `frame` is done: the completion of the frame is reported or the last pass that
/// writes the page signalled (step 6), for an access that `write` says is a write. Returns true when the wait ended at the
/// timeout.
fn wait_complete(pg: usize, frame: u32, write: bool) -> bool {
    let t0 = now_ns();
    let uncommitted = u64::from(frame) > SHARED.committed.load(Ordering::Acquire) & FRAME_MASK;
    let mut waited = false;
    let mut timed_out = false;
    loop {
        let seen = SHARED.wake.load(Ordering::Acquire);
        if !page_flying(pg, frame) {
            break;
        }
        waited = true;
        if now_ns().saturating_sub(t0) > TIMEOUT_NS {
            timed_out = true;
            break;
        }
        sleep_on_wake(seen);
    }
    if waited {
        let ns = now_ns().saturating_sub(t0);
        let k = usize::from(write);
        SHARED.writer_waits.fetch_add(1, Ordering::Relaxed);
        SHARED.writer_wait_ns.fetch_add(ns, Ordering::Relaxed);
        SHARED.wait_n[k].fetch_add(1, Ordering::Relaxed);
        SHARED.wait_ns[k].fetch_add(ns, Ordering::Relaxed);
        SHARED.wait_max_ns[k].fetch_max(ns, Ordering::Relaxed);
        if uncommitted {
            SHARED.wait_uncommitted[k].fetch_add(1, Ordering::Relaxed);
        }
        // The frame still runs when the pass signal ended the wait.
        let by_pass = usize::from(!(in_flight(frame) && !timed_out));
        SHARED.wait_end[by_pass].fetch_add(1, Ordering::Relaxed);
        SHARED.wait_end_ns[by_pass].fetch_add(ns, Ordering::Relaxed);
    }
    if timed_out {
        SHARED.wait_timeouts.fetch_add(1, Ordering::Relaxed);
    }
    timed_out
}

/// Copies the stale blocks of the mirror page `pg` into guest memory: the blocks that a frame may have written. The
/// other blocks of the guest page hold the CPU's bytes (the handler stores to them without waiting) and stay, and so do
/// the blocks that the CPU wrote while a frame wrote them (results one frame late). The guest page must be writable.
/// `frame` is the frame whose bytes these are.
fn copy_from_mirror(pg: usize, mirror: u64, frame: u32) {
    copy_stale(pg, mirror, SHARED.base.load(Ordering::Acquire), true, frame);
}

/// [`copy_from_mirror`] into the page at `dst_base + pg * page` (the guest range itself, or its alias), counted as a copy
/// of the handler when `count` says so.
fn copy_stale(pg: usize, mirror: u64, dst_base: u64, count: bool, frame: u32) {
    let (stale, keep) = info(pg).map_or(([u64::MAX; 4], [0; 4]), |i| (i.stale(), i.keep(frame)));
    copy_mask(pg, mirror, dst_base, &std::array::from_fn(|i| stale[i] & !keep[i]));
    let kept: u32 = stale.iter().zip(&keep).map(|(s, k)| (s & k).count_ones()).sum();
    if kept > 0 {
        SHARED.late_kept.fetch_add(u64::from(kept), Ordering::Relaxed);
    }
    if count {
        SHARED.synced.fetch_add(1, Ordering::Relaxed);
    }
}

/// Copies the blocks of `mask` of the mirror page `pg` into the page at `dst_base + pg * page`.
fn copy_mask(pg: usize, mirror: u64, dst_base: u64, mask: &Mask) {
    let p = page();
    let blocks = (p / BLOCK) as usize;
    let mut b = 0;
    while b < blocks {
        if mask[b / 64] >> (b % 64) & 1 == 0 {
            b += 1;
            continue;
        }
        let first = b;
        while b < blocks && mask[b / 64] >> (b % 64) & 1 != 0 {
            b += 1;
        }
        let off = pg as u64 * p + first as u64 * BLOCK;
        // SAFETY: the blocks lie in the guest page, a whole host page inside the range that `attach` named; the caller
        // made the destination writable (the page itself, or its alias, which is always writable); the mirror blocks at
        // the same offset lie in the reservation of the mirror, which is readable while the page is a member. The two
        // regions are different mappings.
        unsafe {
            std::ptr::copy_nonoverlapping(
                (mirror + off) as *const u8,
                (dst_base + off) as *mut u8,
                (b - first) * BLOCK as usize,
            );
        }
    }
}

/// Serves a fault on the tracked page `pg` when there is a mirror. A page tagged with a frame holds stale bytes: the
/// thread waits for the frame to complete (an internal thread does not wait), copies the page from the mirror and opens
/// it. Atomics, `mprotect`, `memcpy` and the wake word; no lock, no allocation.
fn serve_mirrored(pg: usize, w: &AtomicU32, write: bool, internal: bool, mirror: u64, at: u64) -> FaultKind {
    let p = pg as u32;
    let mut kind: Option<FaultKind> = None;
    let mut gave_up = false;
    loop {
        let cur = w.load(Ordering::Acquire);
        let state = cur & STATE_MASK;
        let frame = cur >> FRAME_SHIFT;
        match state {
            CLEAN => {
                if write {
                    if w.compare_exchange(cur, DIRTY, Ordering::AcqRel, Ordering::Acquire)
                        .is_err()
                    {
                        continue;
                    }
                    log_dirty(pg);
                    protect_pages(p, p + 1, PROT_RW, false);
                    return kind.unwrap_or(FaultKind::CleanWrite);
                }
                protect_pages(p, p + 1, PROT_READ, false);
                return kind.unwrap_or(FaultKind::Other);
            }
            GPU | OPEN => {
                let flying = page_flying(pg, frame) && !gave_up;
                if kind.is_none() && flying && !internal {
                    learn_reader(pg, frame);
                    let n = SHARED.flight_n.fetch_add(1, Ordering::AcqRel) as usize;
                    if n < FLIGHT_CAP {
                        let off = at.wrapping_sub(SHARED.base.load(Ordering::Relaxed)) & ((1 << 35) - 2);
                        let v = u64::from(frame) << 36 | u64::from(write) << 35 | (off + 1);
                        SHARED.flight_log[n].store(v, Ordering::Release);
                    }
                }
                let k = *kind.get_or_insert(if flying {
                    if write {
                        FaultKind::WriteInFlight
                    } else {
                        FaultKind::ReadInFlight
                    }
                } else {
                    FaultKind::Sync
                });
                if state == OPEN && !write {
                    // Open for reading already: a race with the thread that opened it.
                    protect_pages(p, p + 1, PROT_READ, false);
                    return k;
                }
                if flying && !internal {
                    gave_up = wait_complete(pg, frame, write);
                    continue;
                }
                // The claim: one thread copies, the others sleep on the wake word and run their access again.
                if w.compare_exchange(cur, pack(SYNC, u64::from(frame)), Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
                {
                    continue;
                }
                let fresh = cur & FRESH != 0;
                protect_pages(p, p + 1, PROT_RW, false);
                if !(flying && fresh) {
                    copy_from_mirror(pg, mirror, frame);
                    if !flying && !internal && let Some(i) = info(pg) {
                        // A game thread waited for this copy: the completion of the next frame that writes the page copies
                        // it on its own (step 5).
                        i.eager.store(SHARED.epoch.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
                    }
                    if !flying && let Some(i) = info(pg) {
                        // Guest memory is current here again. (A page read in flight stays stale: the frame may
                        // still write it.)
                        for a in &i.stale {
                            a.store(0, Ordering::Release);
                        }
                        // Guest memory holds the frame's bytes: the next frame's readers may go on without a wait.
                        i.snap.store(frame.max(1), Ordering::Release);
                    }
                }
                if flying {
                    SHARED.internal_opens.fetch_add(1, Ordering::Relaxed);
                } else if !internal && in_flight(frame) {
                    // The frame runs on, but the last pass that writes this page is done (step 6).
                    SHARED.early_syncs.fetch_add(1, Ordering::Relaxed);
                }
                if write {
                    w.store(DIRTY, Ordering::Release);
                    log_dirty(pg);
                } else if flying {
                    let open = pack(OPEN, u64::from(frame)) | (cur & FRESH);
                    w.store(open, Ordering::Release);
                    protect_pages(p, p + 1, PROT_READ, false);
                } else {
                    w.store(CLEAN, Ordering::Release);
                    protect_pages(p, p + 1, PROT_READ, false);
                }
                wake_all();
                return k;
            }
            SYNC => {
                // Another thread copies the page. Sleep on the wake word and run the access again.
                sleep_on_wake(SHARED.wake.load(Ordering::Acquire));
                return kind.unwrap_or(FaultKind::Other);
            }
            _ => {
                // Dirty (a race with a flush), or free (a race with a release): open it.
                protect_pages(p, p + 1, PROT_RW, false);
                return kind.unwrap_or(FaultKind::Other);
            }
        }
    }
}

extern "C" fn on_signal(sig: c_int, info: *mut SigInfo, uctx: *mut c_void) {
    // SAFETY: the kernel passes a valid `siginfo_t`.
    let addr = unsafe { info.as_ref() }.map_or(0, |i| i.addr as u64);
    let base = SHARED.base.load(Ordering::Acquire);
    let len = SHARED.len.load(Ordering::Acquire);
    if addr >= base && addr - base < len {
        let pg = ((addr - base) / page()) as usize;
        if let Some(w) = word(pg)
            && w.load(Ordering::Acquire) & STATE_MASK != FREE
        {
            let t0 = now_ns();
            SHARED.last_signal.store(sig as u64, Ordering::Relaxed);
            let (pc, write) = fault_regs(uctx);
            on_tracked(pg, w, pc, write, addr, uctx);
            SHARED
                .handler_ns
                .fetch_add(now_ns().saturating_sub(t0), Ordering::Relaxed);
            return;
        }
    }
    chain(sig, info, uctx);
}

/// Who faulted, by the thread and the program counter.
fn origin_of(pc: u64, internal: bool) -> FaultOrigin {
    if internal {
        return FaultOrigin::Internal;
    }
    let b = SHARED.base.load(Ordering::Relaxed);
    if pc >= b && pc - b < SHARED.len.load(Ordering::Relaxed) {
        FaultOrigin::Guest
    } else {
        FaultOrigin::Host
    }
}

/// Serves a fault on the tracked page `pg`. Atomics, `mprotect` and nothing else (the emulation of an access adds the
/// access itself).
fn on_tracked(pg: usize, w: &AtomicU32, pc: u64, write: bool, addr: u64, uctx: *mut c_void) {
    let internal = is_internal();
    let origin = origin_of(pc, internal);
    let mirror = SHARED.mirror.load(Ordering::Acquire);
    if mirror != 0 {
        if !internal && try_emulate(pg, w, pc, addr, uctx) {
            return;
        }
        let kind = serve_mirrored(pg, w, write, internal, mirror, addr);
        SHARED.faults[kind as usize][origin as usize].fetch_add(1, Ordering::Relaxed);
        if !internal {
            count_pc(kind, pc);
        }
        let base = SHARED.base.load(Ordering::Relaxed);
        if write && matches!(kind, FaultKind::CleanWrite | FaultKind::WriteInFlight) {
            let (ph, newest) = phase_now();
            SHARED.phase_writes[ph].fetch_add(1, Ordering::Relaxed);
            if ph == 1 {
                let r = newest as usize % P1_RING;
                let n = SHARED.p1_n[r].fetch_add(1, Ordering::AcqRel) as usize;
                if n < P1_CAP {
                    SHARED.p1_log[r][n].store(addr.wrapping_sub(base) + 1, Ordering::Release);
                }
                if !internal {
                    count_pc_in(3, pc);
                }
            }
        }
        return;
    }
    let p = pg as u32;
    let kind = loop {
        let cur = w.load(Ordering::Acquire);
        let state = cur & STATE_MASK;
        let flying = state == GPU && in_flight(cur >> FRAME_SHIFT);
        if write && (state == CLEAN || state == GPU) {
            // The CPU writes the page: it is dirty from here until the next flush.
            if w.compare_exchange(cur, DIRTY, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                continue;
            }
            log_dirty(pg);
            protect_pages(p, p + 1, PROT_RW, false);
            break if flying {
                FaultKind::WriteInFlight
            } else if state == CLEAN {
                FaultKind::CleanWrite
            } else {
                FaultKind::Other
            };
        }
        if state == GPU {
            // A read: the page opens for reading and keeps its state, so that a write is seen at its own fault.
            protect_pages(p, p + 1, PROT_READ, false);
            break if flying {
                FaultKind::ReadInFlight
            } else {
                FaultKind::Other
            };
        }
        // A read of a clean page or an access to a dirty one cannot fault; it is a race with a flush that protected
        // the page again. Open it.
        protect_pages(p, p + 1, PROT_RW, false);
        break FaultKind::Other;
    };
    SHARED.faults[kind as usize][origin as usize].fetch_add(1, Ordering::Relaxed);
    if !internal {
        count_pc(kind, pc);
    }
}

fn chain(sig: c_int, info: *mut SigInfo, uctx: *mut c_void) {
    let i = usize::from(sig == SIGSEGV);
    let old = &SHARED.old[i];
    let handler = old.handler.load(Ordering::Acquire);
    let flags = old.flags.load(Ordering::Acquire);
    if handler > 1 && flags & SA_SIGINFO as u32 != 0 {
        // SAFETY: the value is the `sa_sigaction` of a handler installed with SA_SIGINFO, which has this shape.
        let f: Handler = unsafe { std::mem::transmute::<usize, Handler>(handler) };
        f(sig, info, uctx);
        return;
    }
    // The previous action was the default, ignore or a plain handler: put it back and return, so the fault happens
    // again and ends as it would have without this handler.
    let act = SigAction {
        handler,
        mask: 0,
        flags: flags as c_int,
    };
    // SAFETY: `act` is the action `sigaction` gave for this signal before.
    unsafe { sigaction(sig, &act, std::ptr::null_mut()) };
}

fn install_handler() {
    INSTALL.call_once(|| {
        let act = SigAction {
            handler: on_signal as Handler as usize,
            mask: 0,
            flags: SA_SIGINFO | SA_ONSTACK,
        };
        for sig in [SIGBUS, SIGSEGV] {
            let mut old = SigAction {
                handler: 0,
                mask: 0,
                flags: 0,
            };
            // SAFETY: a null action only reads the current one into `old`.
            unsafe { sigaction(sig, std::ptr::null(), &mut old) };
            let i = usize::from(sig == SIGSEGV);
            SHARED.old[i].handler.store(old.handler, Ordering::Release);
            SHARED.old[i]
                .flags
                .store(old.flags as u32, Ordering::Release);
            // SAFETY: `act` is a valid action whose handler is an `extern "C"` function of the SA_SIGINFO shape that
            // lives for the whole process.
            unsafe { sigaction(sig, &act, std::ptr::null_mut()) };
        }
    });
}

static PC_NAMER: AtomicUsize = AtomicUsize::new(0);

/// Sets the function that names a program counter in a guest module (`module+offset`), None outside the modules. The
/// host sets it once the modules are loaded.
pub fn set_pc_namer(f: fn(u64) -> Option<String>) {
    PC_NAMER.store(f as usize, Ordering::Release);
}

/// A name for a program counter: `module+offset` of a guest module, else the host image and symbol, else the number.
pub fn name_pc(pc: u64) -> String {
    let h = PC_NAMER.load(Ordering::Acquire);
    if h != 0 {
        // SAFETY: the value is the address of an `fn(u64) -> Option<String>` that `set_pc_namer` stored; function
        // pointers and `usize` have the same size.
        let f = unsafe { std::mem::transmute::<usize, fn(u64) -> Option<String>>(h) };
        if let Some(n) = f(pc) {
            return n;
        }
    }
    let mut info = DlInfo {
        fname: std::ptr::null(),
        fbase: 0,
        sname: std::ptr::null(),
        saddr: 0,
    };
    // SAFETY: `info` is a valid `Dl_info`; `dladdr` only looks the address up.
    if pc != 0 && unsafe { dladdr(pc as *const c_void, &mut info) } != 0 && !info.fname.is_null() {
        // SAFETY: dyld gives NUL-terminated strings that live while the image is loaded.
        let file = unsafe { std::ffi::CStr::from_ptr(info.fname) }.to_string_lossy();
        let file = file.rsplit('/').next().unwrap_or("?").to_string();
        let sym = if info.sname.is_null() {
            String::new()
        } else {
            // SAFETY: as above.
            let name = unsafe { std::ffi::CStr::from_ptr(info.sname) };
            format!(" {}", name.to_string_lossy())
        };
        return format!("host:{file}+{:#x}{sym}", pc - info.fbase as u64);
    }
    format!("{pc:#x}")
}

/// Makes a second mapping of `[base, base + len)` with read and write access that shares the pages of the first (the
/// pages are the same VM object, so a store through one is a store through the other, and the protection of the two
/// mappings is independent). Every VM region of the range is mapped by itself, so holes and regions without write
/// permission leave a gap in the alias instead of failing the whole call. Returns the address of the byte at `base`.
fn build_alias(base: u64, len: u64) -> Result<u64, String> {
    // SAFETY: an anonymous reservation without access; the kernel picks the address.
    let target = unsafe {
        mmap(
            std::ptr::null_mut(),
            len as usize,
            PROT_NONE,
            MAP_PRIVATE | MAP_ANON | MAP_NORESERVE,
            -1,
            0,
        )
    };
    if target as isize == -1 {
        return Err("no address range for the alias".to_string());
    }
    let target = target as u64;
    let end = base + len;
    let (mut at, mut mapped) = (base, 0u64);
    while at < end {
        let Some(r) = crate::vmregion::region_at_or_after(at) else {
            break;
        };
        if r.start >= end {
            break;
        }
        let (s, e) = (r.start.max(at), (r.start + r.size).min(end));
        at = e;
        if e <= s {
            continue;
        }
        let before = (r.object, r.offset_of(s));
        let (mut t, mut cur, mut max) = (target + (s - base), 0, 0);
        // SAFETY: the source range lies in this task; the target range is inside the reservation made above, which the
        // call replaces with the second mapping (no copy: the pages are shared).
        let kr = unsafe {
            mach_vm_remap(
                mach_task_self_,
                &mut t,
                e - s,
                0,
                VM_FLAGS_FIXED | VM_FLAGS_OVERWRITE,
                mach_task_self_,
                s,
                0,
                &mut cur,
                &mut max,
                VM_INHERIT_NONE,
            )
        };
        if kr != 0 || t != target + (s - base) {
            continue;
        }
        // SAFETY: the range was mapped by the call above.
        let ok = unsafe { mach_vm_protect(mach_task_self_, t, e - s, 0, PROT_RW) } == 0;
        if !ok {
            continue;
        }
        // The first mapping must be the same object after the call (a copy would leave the GPU on the old pages). A region
        // that was never touched has no object yet (id 0): the call makes one, and no page can be under the GPU.
        let after = crate::vmregion::vm_region(s).map(|r| (r.object, r.offset_of(s)));
        if before.0 != 0 && after != Some(before) {
            // SAFETY: the reservation made above.
            unsafe { munmap(target as *mut c_void, len as usize) };
            return Err("the remap moved the guest pages to another VM object".to_string());
        }
        mapped += e - s;
    }
    if mapped == 0 {
        // SAFETY: the reservation made above.
        unsafe { munmap(target as *mut c_void, len as usize) };
        return Err("no region of guest memory could be mapped twice".to_string());
    }
    Ok(target)
}

/// The numbers of the tracker since the process started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PageStats {
    /// The pages that are tracked now.
    pub members: u64,
    /// Faults by `[kind][origin]`, with the kinds of [`FaultKind`] and the origins of [`FaultOrigin`].
    pub faults: [[u64; 3]; 5],
    /// Time that the handler ran for tracked pages.
    pub handler_ns: u64,
    /// Accesses of system calls to pages of a frame in flight (the hook): read, write.
    pub host_access: [u64; 2],
    /// Pages that went dirty through the hook.
    pub hook_dirtied: u64,
    /// Calls of `mprotect`, the time of those outside the handler, and the calls that failed.
    pub mprotects: u64,
    /// Time in `mprotect` outside the handler.
    pub mprotect_ns: u64,
    /// `mprotect` calls that failed.
    pub mprotect_failed: u64,
    /// Dirty pages that did not fit the log; the flush then scans the table.
    pub log_overflows: u64,
    /// Program counters that did not fit the tables.
    pub pc_overflow: u64,
    /// The signal of the last tracked fault: 10 `SIGBUS`, 11 `SIGSEGV`, 0 for none.
    pub last_signal: u64,
    /// Pages copied from the mirror into guest memory (mirrored mode).
    pub synced: u64,
    /// Waits for a writer that still ran, and the time they took.
    pub writer_waits: u64,
    /// Time of those waits.
    pub writer_wait_ns: u64,
    /// Accesses of internal threads to pages in flight, served without a wait.
    pub internal_opens: u64,
    /// Waits that ended at the timeout, with the page copied anyway.
    pub wait_timeouts: u64,
    /// Writes to member pages (first write of a page) by where the newest frame was: after its GPU work or before the
    /// next flush, between its flush and its commit, after its commit and before the GPU is done.
    pub phase_writes: [u64; 3],
    /// Waits of a thread for the writer of a page by access (0 read, 1 write), and their time.
    pub waits: [u64; 2],
    /// Time of those waits.
    pub wait_ns: [u64; 2],
    /// How many of those waits began before the frame was committed.
    pub wait_uncommitted: [u64; 2],
    /// Accesses the handler did itself, by [`Form`], and the time they took in the handler.
    pub emulated: [u64; Form::COUNT],
    /// Time of the emulated accesses.
    pub emulated_ns: u64,
    /// Accesses of a page in flight that the handler could not do, by [`Reason`]; they waited for the writer.
    pub fallbacks: [u64; Reason::COUNT],
    /// Pages the flush staged because the handler stored to them.
    pub emu_staged: u64,
    /// Pages the eager copy moved after a frame completed, `mprotect` calls it made and the time it took (on its own thread).
    pub eager_pages: u64,
    /// `mprotect` calls of the eager copy.
    pub eager_runs: u64,
    /// Time of the eager copy.
    pub eager_ns: u64,
    /// Waits for a writer by what ended them (0 the pass signal of the page while the frame still ran, 1 the completion of
    /// the whole frame or the timeout), and their time.
    pub wait_ends: [u64; 2],
    /// Time of those waits.
    pub wait_end_ns: [u64; 2],
    /// Calls of [`Pages::release_early`], the pages they released, and the accesses that found a page released while its frame
    /// still ran (they copied the page without a wait).
    pub early_calls: u64,
    /// Pages released by the pass signals.
    pub early_pages: u64,
    /// Accesses that found their page released.
    pub early_syncs: u64,
    /// The pages that are CPU-read GPU results now, and how often the set changed (a page joined or left).
    pub readers: u64,
    /// Changes of that set.
    pub reader_changes: u64,
    /// Results one frame late: accesses served from guest memory without a wait to blocks a frame in flight writes, loads
    /// and stores (a store of an atomic counts as a store).
    pub late_reads: u64,
    /// Late stores.
    pub late_writes: u64,
    /// The age of the bytes late accesses met, in frames, summed, and as a histogram of 1, 2, 3 and 4 or more frames.
    pub late_age_sum: u64,
    /// The histogram.
    pub late_age: [u64; 4],
    /// Blocks that late stores took from the frame in flight (the CPU's bytes win over its copy-back).
    pub late_owned: u64,
    /// Blocks a copy from the mirror left alone because the CPU wrote them.
    pub late_kept: u64,
    /// Pages and blocks the completion of a frame copied into guest memory for the next frame's readers, and the pages the
    /// flush copied itself.
    pub late_copied: u64,
    /// Blocks of those copies.
    pub late_copied_blocks: u64,
    /// Pages the flush copied.
    pub late_flush_copies: u64,
    /// Snapshots dropped, by [`SnapWhy`].
    pub snap_resets: [u64; SnapWhy::COUNT],
    /// Late accesses that found no snapshot (they waited), by the reason the page lost it last.
    pub snap_misses: [u64; SnapWhy::COUNT],
    /// Pages the flush left readable while a frame in flight writes them, and the ones that lost it at a completion.
    pub read_open: u64,
    /// Pages that lost the read access at a completion.
    pub read_revoked: u64,
    /// Pages of system call reads (outside the stale blocks, of a page with a snapshot) that went on without a wait.
    pub settle_late: u64,
    /// Pages of system calls that waited for a writer.
    pub settle_waits: u64,
}

/// What one flush did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FlushStats {
    /// The pages that were dirty and went back to clean.
    pub dirtied: u32,
    /// The pages the new frame may write, and the runs they form.
    pub written: u32,
    /// Runs of the pages the new frame may write.
    pub written_runs: u32,
    /// Pages that became tracked in this flush.
    pub new_members: u32,
    /// `mprotect` calls and their time.
    pub mprotects: u32,
    /// Time in the flush.
    pub ns: u64,
    /// The log overflowed and the flush scanned the table.
    pub scanned: bool,
    /// Pages handed to the stager (mirrored mode).
    pub staged: u32,
    /// Pages that the CPU wrote between the staging and the tag (mirrored mode); they are staged again.
    pub raced: u32,
    /// Pages staged because the handler stored to them while a frame wrote others of their blocks (part of `staged`).
    pub emu_staged: u32,
    /// Step 5: pages that stay writable after this flush, the ones among them staged (part of `staged`) and the ones
    /// found unchanged, the pages that became writable and the ones protected again.
    pub hot: u32,
    /// Writable pages that differed from the bytes staged last and were staged.
    pub hot_staged: u32,
    /// Writable pages that were unchanged.
    pub hot_same: u32,
    /// Pages that became writable in this flush.
    pub promoted: u32,
    /// Pages that were protected again in this flush.
    pub demoted: u32,
    /// Where the flush spent its time (mirrored mode): new members, the log of dirty pages, staging with the protection of
    /// the dirty pages, the writable pages, and the tags of the pages the frame writes.
    pub new_ns: u64,
    /// Time collecting the dirty pages from the log.
    pub log_ns: u64,
    /// Time protecting and staging the dirty pages.
    pub stage_ns: u64,
    /// Time diffing the writable pages.
    pub hot_ns: u64,
    /// Time tagging the pages the frame writes.
    pub tag_ns: u64,
    /// Time in `mprotect` during the flush (part of the times above).
    pub mprotect_ns: u64,
}

/// The page tracker of the process. There is one, because the signal handler needs its table.
#[derive(Clone, Copy, Debug)]
pub struct Pages {
    _private: (),
}

static PAGES: Pages = Pages { _private: () };

/// Merges sorted page numbers into runs `(first, end)`.
fn runs_of(sorted: &[u32]) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for &p in sorted {
        match out.last_mut() {
            Some(last) if p == last.1 => last.1 = p + 1,
            Some(last) if p < last.1 => {}
            _ => out.push((p, p + 1)),
        }
    }
    out
}

/// Page runs of the host ranges `(address, length)`, sorted and merged.
fn span_runs(ranges: &[(u64, u64)]) -> Vec<(u32, u32)> {
    let mut v: Vec<(u32, u32)> = ranges
        .iter()
        .filter_map(|&(a, l)| page_span(a, l))
        .collect();
    v.sort_unstable();
    let mut out: Vec<(u32, u32)> = Vec::with_capacity(v.len());
    for (s, e) in v {
        match out.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// Sets the blocks that the byte offsets `[lo, hi)` of a page touch (rounded outward). `hi` must be above `lo`.
fn set_blocks(m: &mut Mask, lo: u64, hi: u64) {
    let (first, last) = (lo / BLOCK, (hi - 1) / BLOCK);
    // One mask per word of the bits `first..=last` that lie in it.
    for (w, word) in m.iter_mut().enumerate() {
        let (from, to) = (w as u64 * 64, w as u64 * 64 + 63);
        if last < from || first > to {
            continue;
        }
        let (a, b) = (first.max(from) - from, last.min(to) - from);
        let bits = if b - a == 63 { u64::MAX } else { ((1u64 << (b - a + 1)) - 1) << a };
        *word |= bits;
    }
}

/// The blocks that the host ranges `(address, length)` touch in each page of `runs` (the runs of the same ranges, as
/// [`span_runs`] makes them): one mask per page, per run.
fn range_masks(runs: &[(u32, u32)], ranges: &[(u64, u64)]) -> Vec<Vec<Mask>> {
    let mut out: Vec<Vec<Mask>> = runs.iter().map(|&(s, e)| vec![[0; 4]; (e - s) as usize]).collect();
    let base = SHARED.base.load(Ordering::Acquire);
    let total = SHARED.len.load(Ordering::Acquire);
    let npages = SHARED.npages.load(Ordering::Acquire) as u64;
    let p = page();
    for &(a, l) in ranges {
        let lo = a.max(base);
        let hi = a.saturating_add(l.max(1)).min(base.saturating_add(total));
        if lo >= hi {
            continue;
        }
        let (first, last) = ((lo - base) / p, (hi - 1 - base) / p);
        for pg in first..=last.min(npages.saturating_sub(1)) {
            let i = runs.partition_point(|&(_, e)| u64::from(e) <= pg);
            let Some(&(s, e)) = runs.get(i) else { break };
            if pg < u64::from(s) || pg >= u64::from(e) {
                continue;
            }
            let from = if pg == first { (lo - base) % p } else { 0 };
            let to = if pg == last { (hi - 1 - base) % p + 1 } else { p };
            set_blocks(&mut out[i][(pg - u64::from(s)) as usize], from, to);
        }
    }
    out
}

fn in_runs(runs: &[(u32, u32)], p: u32) -> bool {
    let i = runs.partition_point(|&(_, e)| e <= p);
    runs.get(i).is_some_and(|&(s, _)| s <= p)
}

/// The pages `[first, end)` of the frame `frame` that are still protected for it go read only and clean.
fn release_runs(frame: u64, runs: &[(u32, u32)]) {
    let want = pack(GPU, frame);
    for &(first, end) in runs {
        let mut start: Option<u32> = None;
        for p in first..end {
            let done = word(p as usize).is_some_and(|w| {
                w.compare_exchange(want, CLEAN, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            });
            match (done, start) {
                (true, None) => start = Some(p),
                (false, Some(s)) => {
                    protect_pages(s, p, PROT_READ, true);
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(s) = start {
            protect_pages(s, end, PROT_READ, true);
        }
    }
}

/// The pages of `runs` that a host thread opened for reading in frame `frame` lose their access again.
fn close_opened(frame: u64, runs: &[(u32, u32)]) {
    for &(first, end) in runs {
        let mut start: Option<u32> = None;
        for p in first..end {
            let done = word(p as usize).is_some_and(|w| loop {
                let cur = w.load(Ordering::Acquire);
                if cur & STATE_MASK != OPEN || u64::from(cur >> FRAME_SHIFT) != frame & FRAME_MASK {
                    break false;
                }
                let back = pack(GPU, frame) | (cur & FRESH);
                if w.compare_exchange(cur, back, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                    break true;
                }
            });
            match (done, start) {
                (true, None) => start = Some(p),
                (false, Some(s)) => {
                    protect_pages(s, p, PROT_NONE, true);
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(s) = start {
            protect_pages(s, end, PROT_NONE, true);
        }
    }
}

/// True when a system call may read `[addr, addr + len)` of the page `pg` (tagged `GPU` with `w`) without waiting for the
/// writer: results are late, a frame in flight writes the page, its guest bytes are a completed frame's (`snap`, kept
/// current by the completions) and the read touches none of the blocks that frame writes.
fn late_host_read(pg: usize, w: &AtomicU32, addr: u64, len: u64) -> bool {
    if !SHARED.late.load(Ordering::Relaxed) || SHARED.alias.load(Ordering::Acquire) == 0 {
        return false;
    }
    let cur = w.load(Ordering::Acquire);
    if cur & STATE_MASK != GPU || !page_flying(pg, cur >> FRAME_SHIFT) {
        return false;
    }
    let Some(i) = info(pg) else { return false };
    if i.snap.load(Ordering::Acquire) == 0 {
        return false;
    }
    let (base, p) = (SHARED.base.load(Ordering::Relaxed), page());
    let page_lo = base + pg as u64 * p;
    let lo = addr.max(page_lo) - page_lo;
    let hi = addr.saturating_add(len.max(1)).min(page_lo + p) - page_lo;
    if lo >= hi {
        return false;
    }
    let stale = i.stale();
    !(lo / BLOCK..=(hi - 1) / BLOCK).any(|b| stale[(b / 64) as usize] >> (b % 64) & 1 != 0)
}

/// True while game threads read the page `i` as a CPU-read GPU result: they faulted on it for a copy, or read it late, in
/// the last epochs.
fn late_active(i: &PageInfo) -> bool {
    let e = i.eager.load(Ordering::Relaxed);
    e != 0 && SHARED.epoch.load(Ordering::Relaxed) <= e
}

/// Takes the access to the page `pg` away again (`PROT_NONE`) when it is tagged GPU with the word `cur`: the page was left
/// readable for late readers and its guest bytes stop being a completed frame's. The claim keeps a thread that opens the
/// page at the same time from meeting the change halfway.
fn revoke_read(pg: usize, w: &AtomicU32, cur: u32) {
    if w.compare_exchange(cur, pack(SYNC, u64::from(cur >> FRAME_SHIFT)), Ordering::AcqRel, Ordering::Acquire).is_ok() {
        protect_pages(pg as u32, pg as u32 + 1, PROT_NONE, false);
        w.store(cur, Ordering::Release);
        wake_all();
        SHARED.read_revoked.fetch_add(1, Ordering::Relaxed);
    }
}

/// Results one frame late, at the completion of `frame` (the caller holds the control mutex): the pages of the frame's
/// `runs` whose guest bytes are the last completed frame's get the blocks that `frame` wrote copied from the mirror into
/// guest memory through the alias, so that guest memory is the bytes of `frame` for the readers of the frame that runs now
/// (a newer frame already tags the page) or of the next one (`frame` is the newest tag: the flush of the next frame comes
/// after this completion when the CPU runs one frame ahead, and the copy-back thread is not there yet). The CPU's own
/// blocks stay. A page that no game thread read lately loses its place, and with it the read access that the flush gave it.
fn late_copy_back(frame: u64, runs: &[(u32, u32)], masks: Option<&[Vec<Mask>]>, mirror: u64, alias: u64) {
    let (mut pages, mut blocks) = (0u64, 0u64);
    for (ri, &(first, end)) in runs.iter().enumerate() {
        for pg in first..end {
            let Some(w) = word(pg as usize) else { continue };
            let cur = w.load(Ordering::Acquire);
            let state = cur & STATE_MASK;
            let Some(i) = info(pg as usize) else { continue };
            if matches!(state, SYNC | OPEN) {
                // A thread copies or reads the page right now: its bytes are no longer the last completed frame's.
                i.snap_reset(SnapWhy::Busy);
                continue;
            }
            let tag = u64::from(cur >> FRAME_SHIFT);
            if state != GPU || tag < frame & FRAME_MASK {
                continue;
            }
            // The newest tag is `frame` itself: no newer frame writes the page.
            let newest = tag == frame & FRAME_MASK;
            if i.snap.load(Ordering::Acquire) == 0 {
                continue;
            }
            let m = masks.and_then(|m| m.get(ri)).and_then(|r| r.get((pg - first) as usize));
            let Some(m) = m.filter(|_| late_active(i)) else {
                // The blocks of the frame are not known, or nobody reads the page: guest memory is not kept up to date.
                let why = if m.is_none() { SnapWhy::Unknown } else { SnapWhy::Idle };
                i.snap_reset(why);
                revoke_read(pg as usize, w, cur);
                continue;
            };
            let f = (frame & FRAME_MASK) as u32;
            // For the newest tag the page is claimed while it is copied: a thread that faults on it meanwhile (it is
            // complete, so the thread copies it itself and may write) waits for the claim instead of racing the copy.
            if newest && w.compare_exchange(cur, pack(SYNC, frame), Ordering::AcqRel, Ordering::Acquire).is_err() {
                continue;
            }
            let keep = i.keep(f);
            let copy: Mask = std::array::from_fn(|k| m[k] & !keep[k]);
            let kept: u32 = m.iter().zip(&keep).map(|(a, b)| (a & b).count_ones()).sum();
            copy_mask(pg as usize, mirror, alias, &copy);
            i.snap.store(f.max(1), Ordering::Release);
            if newest {
                w.store(cur, Ordering::Release);
                wake_all();
            }
            pages += 1;
            blocks += u64::from(copy.iter().map(|c| c.count_ones()).sum::<u32>());
            if kept > 0 {
                SHARED.late_kept.fetch_add(u64::from(kept), Ordering::Relaxed);
            }
        }
    }
    SHARED.late_copied.fetch_add(pages, Ordering::Relaxed);
    SHARED.late_copied_blocks.fetch_add(blocks, Ordering::Relaxed);
}

/// The blocks a frame may write, by run and page.
type FrameMasks = (u64, Vec<Vec<Mask>>);

/// A completed frame and the page runs it protected.
type FrameRuns = (u64, Vec<(u32, u32)>);

/// The completed frames whose pages the eager thread looks at, oldest first.
struct EagerQueue {
    jobs: Mutex<VecDeque<FrameRuns>>,
    wake: std::sync::Condvar,
}

static EAGER: EagerQueue = EagerQueue {
    jobs: Mutex::new(VecDeque::new()),
    wake: std::sync::Condvar::new(),
};
static EAGER_THREAD: Once = Once::new();
/// True while the eager thread holds a job (set under the queue lock when it takes one).
static EAGER_BUSY: AtomicBool = AtomicBool::new(false);

/// Drops the queued jobs and waits for the one in progress: a new `attach` replaces the tables and the host may unmap the
/// guest range the job writes through the alias.
fn eager_quiesce() {
    EAGER.jobs.lock().unwrap_or_else(PoisonError::into_inner).clear();
    while EAGER_BUSY.load(Ordering::Acquire) {
        std::thread::yield_now();
    }
}
/// The most completed frames that wait for the eager thread; older ones are dropped (their pages copy on a fault).
const EAGER_QUEUE_MAX: usize = 16;

fn eager_submit(jobs: Vec<FrameRuns>) {
    EAGER_THREAD.call_once(|| {
        let _ = std::thread::Builder::new().name("nxkit-pages-copyback".into()).spawn(|| {
            loop {
                let job = {
                    let mut q = EAGER.jobs.lock().unwrap_or_else(PoisonError::into_inner);
                    while q.is_empty() {
                        q = EAGER.wake.wait(q).unwrap_or_else(PoisonError::into_inner);
                    }
                    let job = q.pop_front();
                    if job.is_some() {
                        EAGER_BUSY.store(true, Ordering::Release);
                    }
                    job
                };
                if let Some((f, runs)) = job {
                    eager_copy(f, &runs);
                    EAGER_BUSY.store(false, Ordering::Release);
                }
            }
        });
    });
    let mut q = EAGER.jobs.lock().unwrap_or_else(PoisonError::into_inner);
    q.extend(jobs);
    while q.len() > EAGER_QUEUE_MAX {
        q.pop_front();
    }
    drop(q);
    EAGER.wake.notify_one();
}

/// The eager copy of the completed frame `frame`: the pages of `runs` that are still tagged with it, that a game thread
/// faulted on for a copy in the last epochs and that nobody touched since are copied from the mirror through the alias,
/// opened for reading and set clean, in the background, so that the game thread does not take the fault. It holds the
/// control mutex per run, so a flush never sees a page in the middle of it.
fn eager_copy(frame: u64, runs: &[(u32, u32)]) {
    let alias = SHARED.alias.load(Ordering::Acquire);
    let mirror = SHARED.mirror.load(Ordering::Acquire);
    if alias == 0 || mirror == 0 || !SHARED.eager_on.load(Ordering::Acquire) {
        return;
    }
    let t0 = now_ns();
    let epoch = SHARED.epoch.load(Ordering::Relaxed);
    let (mut pages, mut calls) = (0u64, 0u64);
    for &(first, end) in runs {
        let _c = ctrl();
        let mut run: Option<(u32, u32)> = None;
        let finish = |r: (u32, u32), calls: &mut u64| {
            protect_pages(r.0, r.1, PROT_READ, false);
            *calls += 1;
            for pg in r.0..r.1 {
                if let Some(w) = word(pg as usize) {
                    w.store(CLEAN, Ordering::Release);
                }
            }
            wake_all();
        };
        for pg in first..end {
            let claimed = (|| {
                let i = info(pg as usize)?;
                let e = i.eager.load(Ordering::Relaxed);
                if e == 0 || epoch > e {
                    return None;
                }
                let w = word(pg as usize)?;
                let cur = w.load(Ordering::Acquire);
                if cur & STATE_MASK != GPU || u64::from(cur >> FRAME_SHIFT) != frame & FRAME_MASK {
                    return None;
                }
                w.compare_exchange(cur, pack(SYNC, frame), Ordering::AcqRel, Ordering::Acquire).ok()?;
                copy_stale(pg as usize, mirror, alias, false, frame as u32);
                for a in &i.stale {
                    a.store(0, Ordering::Release);
                }
                i.snap.store((frame as u32).max(1), Ordering::Release);
                Some(())
            })()
            .is_some();
            match (claimed, run) {
                (true, None) => run = Some((pg, pg + 1)),
                (true, Some((s, _))) => run = Some((s, pg + 1)),
                (false, Some(r)) => {
                    finish(r, &mut calls);
                    run = None;
                }
                (false, None) => {}
            }
            pages += u64::from(claimed);
        }
        if let Some(r) = run {
            finish(r, &mut calls);
        }
    }
    SHARED.eager_pages.fetch_add(pages, Ordering::Relaxed);
    SHARED.eager_runs.fetch_add(calls, Ordering::Relaxed);
    SHARED.eager_ns.fetch_add(now_ns().saturating_sub(t0), Ordering::Relaxed);
}

/// Receives the guest pages that the CPU wrote, to carry them into the mirror on the GPU.
pub trait Stager {
    /// At most `max_pages` pages follow.
    fn begin(&mut self, max_pages: usize);
    /// The bytes of the guest page `page`. A page that is staged twice keeps the later bytes.
    fn stage(&mut self, page: u32, bytes: &[u8]);
    /// Like [`stage`](Self::stage), for a page whose bytes in the blocks of `skip` (bit `b % 64` of word `b / 64` is the
    /// block `b` of 64 bytes) are stale and must not reach the mirror: a frame in flight may have written them there.
    fn stage_masked(&mut self, page: u32, bytes: &[u8], skip: [u64; 4]);
}

impl Pages {
    /// The host address of the first tracked page (the guest base rounded up to a page).
    pub fn host_base(&self) -> u64 {
        SHARED.base.load(Ordering::Acquire)
    }

    /// The page numbers (first, end) of the host range, for callers that keep their own tables.
    pub fn page_range(&self, addr: u64, len: u64) -> Option<(u32, u32)> {
        page_span(addr, len)
    }

    /// The tracker.
    pub fn global() -> &'static Pages {
        &PAGES
    }

    /// Names the guest range `[base, base + len)`, makes the table and installs the signal handler and the hook of
    /// `nxkit_abi::guest`. The host calls it once, after the crash report is installed, because the handler hands the
    /// faults it does not own to the handler it finds.
    pub fn attach(&self, base: u64, len: u64) {
        eager_quiesce();
        let p = page();
        let (b, e) = (base.div_ceil(p) * p, (base + len) & !(p - 1));
        let len = e.saturating_sub(b);
        let n = (len / p) as usize;
        // A table of zero words: the system gives zero pages that cost nothing until a page is tracked.
        let Ok(layout) = std::alloc::Layout::array::<AtomicU32>(n.max(1)) else {
            return;
        };
        // SAFETY: the layout is not empty; zeroed memory is a valid table of free pages. The table is never freed.
        let table = unsafe { std::alloc::alloc_zeroed(layout) }.cast::<AtomicU32>();
        if table.is_null() {
            return;
        }
        let Ok(bits_layout) = std::alloc::Layout::array::<AtomicU64>(n.div_ceil(64).max(1)) else {
            return;
        };
        // SAFETY: as for the table: a non-empty layout, zeroed memory is a valid bitmap, and it is never freed.
        let bits = unsafe { std::alloc::alloc_zeroed(bits_layout) }.cast::<AtomicU64>();
        if bits.is_null() {
            return;
        }
        let Ok(info_layout) = std::alloc::Layout::array::<PageInfo>(n.max(1)) else {
            return;
        };
        // SAFETY: as for the table: zero bytes are a valid `PageInfo`, and the array is never freed.
        let infos = unsafe { std::alloc::alloc_zeroed(info_layout) }.cast::<PageInfo>();
        if infos.is_null() {
            return;
        }
        SHARED.alias.store(0, Ordering::Release);
        SHARED.info.store(infos, Ordering::Release);
        SHARED.members.store(0, Ordering::Release);
        SHARED.completed.store(0, Ordering::Release);
        SHARED.flushed.store(0, Ordering::Release);
        SHARED.committed.store(0, Ordering::Release);
        SHARED.mirror.store(0, Ordering::Release);
        SHARED.npages.store(0, Ordering::Release);
        SHARED.member_bits.store(bits, Ordering::Release);
        SHARED.member_gen.fetch_add(1, Ordering::AcqRel);
        SHARED.table.store(table, Ordering::Release);
        SHARED.base.store(b, Ordering::Release);
        SHARED.len.store(len, Ordering::Release);
        SHARED.npages.store(n, Ordering::Release);
        SHARED.log_n.store(0, Ordering::Release);
        SHARED.readers_n.store(0, Ordering::Release);
        {
            let mut l = LEARNED.lock().unwrap_or_else(PoisonError::into_inner);
            l.pages.clear();
        }
        {
            let mut c = ctrl();
            c.frames.clear();
            c.masks.clear();
            c.snap = None;
            c.streak = vec![0; n];
            c.prev_dirty.clear();
            c.hot.clear();
        }
        install_handler();
        nxkit_abi::guest::set_guard_hook(hook);
    }

    /// Maps guest memory a second time with read and write access, for the emulation of accesses outside the blocks a frame
    /// in flight writes (step 4). It needs the guest range to be readable and writable, so the host calls it before the
    /// pages are protected, and with the mirror. Returns an error text when it could not (the handler then waits, as in
    /// step 3). A host page of more than 16 KiB cannot be covered by the four-word mask.
    pub fn make_alias(&self) -> Result<(), String> {
        if !self.attached() {
            return Err("the tracker is not attached".to_string());
        }
        if page() > 4 * 64 * BLOCK {
            return Err("the host page has more blocks than the mask holds".to_string());
        }
        let (base, len) = (SHARED.base.load(Ordering::Acquire), SHARED.len.load(Ordering::Acquire));
        let alias = build_alias(base, len)?;
        SHARED.alias.store(alias, Ordering::Release);
        Ok(())
    }

    /// Turns the emulation on or off (on by default once the alias exists); off, every access to a page in flight waits.
    pub fn set_emulation(&self, on: bool) {
        SHARED.emulate.store(on, Ordering::Release);
    }

    /// True when the handler can emulate accesses (the alias exists and the emulation is on).
    pub fn emulating(&self) -> bool {
        SHARED.alias.load(Ordering::Acquire) != 0 && SHARED.emulate.load(Ordering::Acquire)
    }

    /// The bytes of the guest page `page` through the alias, whatever its protection; None without an alias.
    fn alias_page(&self, pg: u32) -> Option<&'static [u8]> {
        let alias = SHARED.alias.load(Ordering::Acquire);
        if alias == 0 {
            return None;
        }
        let p = page();
        // SAFETY: the alias maps the whole guest range with read access and `pg` is a page inside the table; the bytes
        // may change under a reader (the CPU stores through it), which the staging accepts.
        Some(unsafe { std::slice::from_raw_parts((alias + u64::from(pg) * p) as *const u8, p as usize) })
    }

    /// Whether [`attach`](Self::attach) ran.
    pub fn attached(&self) -> bool {
        SHARED.npages.load(Ordering::Acquire) != 0
    }

    /// The host page size.
    pub fn page_size(&self) -> u64 {
        page()
    }

    /// The state of the page that holds the host address: `free`, `clean`, `dirty` or `gpu(frame)` as text, for tests
    /// and reports.
    pub fn state_of(&self, host_addr: u64) -> String {
        let base = SHARED.base.load(Ordering::Acquire);
        let Some(w) = host_addr
            .checked_sub(base)
            .and_then(|o| word((o / page()) as usize))
        else {
            return "outside".to_string();
        };
        let v = w.load(Ordering::Acquire);
        match v & STATE_MASK {
            FREE => "free".to_string(),
            CLEAN => "clean".to_string(),
            DIRTY => "dirty".to_string(),
            SYNC => "sync".to_string(),
            OPEN => format!("open({})", v >> FRAME_SHIFT),
            _ if v & FRESH != 0 => format!("gpu({}, fresh)", v >> FRAME_SHIFT),
            _ => format!("gpu({})", v >> FRAME_SHIFT),
        }
    }

    /// Declares the mirror: the host address of a reservation as large as guest memory, whose byte `o` is the GPU's copy
    /// of guest byte `o`. From now on a page tagged with a frame holds stale guest bytes, and a CPU access to it waits
    /// for the frame and copies the page from the mirror ([`flush_mirrored`](Self::flush_mirrored)). 0 turns it off.
    pub fn set_mirror(&self, host_base: u64) {
        SHARED.mirror.store(host_base, Ordering::Release);
    }

    /// Step 5, off by default: `eager` copies back, after a frame completes and on a thread of its own, the pages that game
    /// threads faulted on lately for a copy from the mirror (it needs the alias); `hot` keeps the pages that the CPU
    /// dirties in many flushes in a row writable and diffs them at each flush instead of faulting on them.
    pub fn set_adaptive(&self, eager: bool, hot: bool) {
        SHARED.eager_on.store(eager, Ordering::Release);
        SHARED.hot_on.store(hot, Ordering::Release);
    }

    /// Results one frame late (needs the alias and the emulation): a game thread's access to the blocks that a frame in
    /// flight writes is served from guest memory, which holds the last completed frame's bytes there, without a wait.
    /// A page qualifies after a game thread has waited for it once. A store goes to guest memory too and wins over the
    /// copy-back of the frame in flight; the next flush stages it into the mirror.
    pub fn set_late(&self, on: bool) {
        SHARED.late.store(on, Ordering::Release);
    }

    /// Whether the flush leaves the pages with a snapshot readable while a frame in flight writes them (needs
    /// [`set_late`](Self::set_late)): a read of such a page takes no trap and gets the last completed frame's bytes.
    pub fn set_read_open(&self, on: bool) {
        SHARED.read_open_on.store(on, Ordering::Release);
    }

    /// True when results come one frame late ([`set_late`](Self::set_late)).
    pub fn late(&self) -> bool {
        SHARED.late.load(Ordering::Acquire)
    }

    /// The largest age in frames of the bytes served late since the last call.
    pub fn take_late_age_max(&self) -> u64 {
        SHARED.late_age_max.swap(0, Ordering::Relaxed)
    }

    /// Stops tracking every page and gives them all read and write access again: for a host that lets the range go, and for
    /// tests whose guest memory goes back to the allocator.
    pub fn release_all(&self) {
        if self.attached() {
            self.forget(SHARED.base.load(Ordering::Acquire), SHARED.len.load(Ordering::Acquire));
        }
    }

    /// Drops the queued eager copies and waits for the one that runs. The host calls it before it unmaps the guest range
    /// (the copy writes through the alias); `attach` does it too.
    pub fn quiesce(&self) {
        eager_quiesce();
    }

    /// The first command buffer of `frame` was committed: the writes of the CPU from here on are no longer "between the
    /// flush and the commit" of that frame.
    pub fn note_committed(&self, frame: u64) {
        SHARED.committed.store(frame, Ordering::Release);
    }

    /// Copies `buf.len()` bytes of guest memory at the host address `addr` into `buf`, when every page of the range is a
    /// member that is readable now (clean or dirty, not tagged for a frame). Returns false otherwise. For the
    /// diagnostics: the pages may change state right after the check.
    pub fn peek(&self, addr: u64, buf: &mut [u8]) -> bool {
        let base = SHARED.base.load(Ordering::Acquire);
        let total = SHARED.len.load(Ordering::Acquire);
        if buf.is_empty() || addr < base || addr.saturating_add(buf.len() as u64) > base.saturating_add(total) {
            return false;
        }
        let Some((first, end)) = page_span(addr, buf.len() as u64) else {
            return false;
        };
        let readable = (first..end).all(|p| {
            word(p as usize).is_some_and(|w| matches!(w.load(Ordering::Acquire) & STATE_MASK, CLEAN | DIRTY))
        });
        if !readable {
            return false;
        }
        // SAFETY: the range lies in the guest memory that `attach` named (checked above) and every page of it is a member
        // that is clean (read only) or dirty (read and write), so the read cannot fault.
        unsafe {
            std::ptr::copy_nonoverlapping(addr as *const u8, buf.as_mut_ptr(), buf.len());
        }
        true
    }

    /// The first accesses of game threads to pages of a frame in flight since the last call, as `(frame, host address,
    /// write)` (at most 1024; the second number is how many there were).
    pub fn take_in_flight(&self) -> (Vec<(u32, u64, bool)>, u32) {
        let base = SHARED.base.load(Ordering::Acquire);
        let n = SHARED.flight_n.swap(0, Ordering::AcqRel);
        let out = SHARED.flight_log[..(n as usize).min(FLIGHT_CAP)]
            .iter()
            .map(|s| s.load(Ordering::Acquire))
            .filter(|&v| v != 0)
            .map(|v| ((v >> 36) as u32, base + (v & ((1 << 35) - 1)) - 1, v >> 35 & 1 != 0))
            .collect();
        (out, n)
    }

    /// The host addresses of the first writes of the CPU to member pages after the flush of `frame` and before its commit,
    /// as logged (at most [`P1_CAP`]; the second number is how many there were). The log of the frame is emptied.
    pub fn take_between(&self, frame: u64) -> (Vec<u64>, u32) {
        let base = SHARED.base.load(Ordering::Acquire);
        let r = frame as usize % P1_RING;
        let n = SHARED.p1_n[r].swap(0, Ordering::AcqRel);
        let out = SHARED.p1_log[r][..(n as usize).min(P1_CAP)]
            .iter()
            .map(|s| s.load(Ordering::Acquire))
            .filter(|&v| v != 0)
            .map(|v| v - 1 + base)
            .collect();
        (out, n)
    }

    /// With a mirror, copies every page of a frame back into guest memory when the frame completes (it makes guest memory
    /// fresh, for bisecting a defect with the knobs that bind guest memory instead of the mirror).
    pub fn set_eager_sync(&self, on: bool) {
        SHARED.eager.store(on, Ordering::Release);
    }

    /// The newest frame whose completion was reported.
    pub fn completed(&self) -> u64 {
        SHARED.completed.load(Ordering::Acquire) & FRAME_MASK
    }

    /// The member bitmap (bit `i` of word `i / 64` is set while page `i` is tracked) and its generation. The copy is
    /// remade only when a page joined or left since the last call.
    pub fn member_snapshot(&self) -> (std::sync::Arc<Vec<u64>>, u64) {
        let gen_ = SHARED.member_gen.load(Ordering::Acquire);
        let mut c = ctrl();
        if let Some((g, a)) = &c.snap
            && *g == gen_
        {
            return (a.clone(), gen_);
        }
        let n = SHARED.npages.load(Ordering::Acquire).div_ceil(64);
        let p = SHARED.member_bits.load(Ordering::Acquire);
        let v: Vec<u64> = if p.is_null() {
            Vec::new()
        } else {
            // SAFETY: the bitmap has `n` words that are never freed.
            (0..n).map(|i| unsafe { &*p.add(i) }.load(Ordering::Acquire)).collect()
        };
        let a = std::sync::Arc::new(v);
        c.snap = Some((gen_, a.clone()));
        (a, gen_)
    }

    /// True when every page of `[addr, addr + len)` is tracked (a member of the private set), so the mirror holds the
    /// bytes the GPU reads there. An empty or untracked range gives false.
    pub fn all_members(&self, addr: u64, len: u64) -> bool {
        let Some((first, end)) = page_span(addr, len) else {
            return false;
        };
        first < end
            && (first..end).all(|p| {
                word(p as usize).is_some_and(|w| w.load(Ordering::Acquire) & STATE_MASK != FREE)
            })
    }

    /// True when a page of the host range `[addr, addr + len)` is tagged with a frame: its guest bytes may be stale.
    pub fn any_gpu_owned(&self, addr: u64, len: u64) -> bool {
        let Some((first, end)) = page_span(addr, len) else {
            return false;
        };
        (first..end).any(|p| {
            word(p as usize).is_some_and(|w| matches!(w.load(Ordering::Acquire) & STATE_MASK, GPU | OPEN | SYNC))
        })
    }

    /// The flush of frame `frame`. First the pages that the CPU dirtied since the last flush go back to clean and read
    /// only, except the ones the new frame may write; then the pages in `written` (host address and length) are
    /// protected against all access and tagged with the frame, becoming tracked if they were not. The frame's runs
    /// are released by [`complete`](Self::complete).
    pub fn flush(&self, frame: u64, written: &[(u64, u64)]) -> FlushStats {
        let mut st = FlushStats::default();
        if !self.attached() {
            return st;
        }
        let t0 = now_ns();
        let calls0 = SHARED.mprotects.load(Ordering::Relaxed);
        let mut c = ctrl();
        let runs = span_runs(written);
        // The pages that went dirty.
        let n = SHARED.log_n.swap(0, Ordering::AcqRel);
        let mut dirty: Vec<u32> = Vec::new();
        if n > LOG_CAP {
            st.scanned = true;
            let total = SHARED.npages.load(Ordering::Acquire);
            for p in 0..total {
                if word(p).is_some_and(|w| w.load(Ordering::Relaxed) == DIRTY) {
                    dirty.push(p as u32);
                }
            }
        } else {
            for slot in &SHARED.log[..n] {
                // A handler that took its index may not have stored yet; it does so within a few instructions.
                let mut spins = 0;
                let v = loop {
                    let v = slot.swap(0, Ordering::AcqRel);
                    if v != 0 || spins > 10_000 {
                        break v;
                    }
                    spins += 1;
                    std::hint::spin_loop();
                };
                if v != 0 {
                    dirty.push(v - 1);
                }
            }
            dirty.sort_unstable();
            dirty.dedup();
        }
        let mut back: Vec<u32> = Vec::with_capacity(dirty.len());
        for p in dirty {
            if in_runs(&runs, p) {
                continue;
            }
            // The state goes first: a store after this CAS and before the protection is in guest memory already, and a
            // store after the protection faults and is logged for the next flush.
            if word(p as usize).is_some_and(|w| {
                w.compare_exchange(DIRTY, CLEAN, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            }) {
                back.push(p);
            }
        }
        st.dirtied = back.len() as u32;
        for (s, e) in runs_of(&back) {
            protect_pages(s, e, PROT_READ, true);
        }
        // The pages of the new frame.
        let mut added = 0u64;
        for &(s, e) in &runs {
            for p in s..e {
                if let Some(w) = word(p as usize)
                    && w.swap(pack(GPU, frame), Ordering::AcqRel) & STATE_MASK == FREE
                {
                    added += 1;
                    member_bit(p as usize, true);
                }
            }
            protect_pages(s, e, PROT_NONE, true);
            st.written += e - s;
        }
        SHARED.members.fetch_add(added, Ordering::AcqRel);
        st.new_members = added as u32;
        st.written_runs = runs.len() as u32;
        if !runs.is_empty() {
            c.frames.push_back((frame, runs));
        }
        while c.frames.len() > MAX_FRAMES {
            if let Some((f, r)) = c.frames.pop_front() {
                release_runs(f, &r);
            }
        }
        st.mprotects = (SHARED.mprotects.load(Ordering::Relaxed) - calls0) as u32;
        st.ns = now_ns().saturating_sub(t0);
        st
    }

    /// The pages that game threads waited for as the writer of a frame lately (within [`LEARN_FRAMES`] frames of `now`), sorted:
    /// CPU-read GPU results. The encoder of a frame cuts the frame after the last pass that writes such a page and calls
    /// [`release_early`](Self::release_early) when that part is done (step 6).
    pub fn readers(&self, now: u64) -> Vec<u32> {
        let mut l = LEARNED.lock().unwrap_or_else(PoisonError::into_inner);
        let n = SHARED.readers_n.swap(0, Ordering::AcqRel).min(READERS_CAP);
        let mut changed = 0u64;
        for slot in &SHARED.readers_log[..n] {
            let v = slot.swap(0, Ordering::AcqRel);
            if v != 0 && l.pages.insert(v - 1) {
                changed += 1;
            }
        }
        let now = now & FRAME_MASK;
        let old: Vec<u32> = l
            .pages
            .iter()
            .copied()
            .filter(|&pg| {
                info(pg as usize).is_none_or(|i| {
                    let w = u64::from(i.waited.load(Ordering::Relaxed));
                    w == 0 || now.saturating_sub(w) > LEARN_FRAMES
                })
            })
            .collect();
        for pg in &old {
            l.pages.remove(pg);
        }
        changed += old.len() as u64;
        l.changes += changed;
        l.pages.iter().copied().collect()
    }

    /// The last pass of `frame` that writes the pages of `runs` (page numbers, first and end) is done on the GPU: a thread that
    /// waits for the writer of one of them goes on, before the rest of the frame completes. Only pages that no later pass
    /// of the frame writes may be named (step 6).
    pub fn release_early(&self, frame: u64, runs: &[(u32, u32)]) {
        let frame = (frame & FRAME_MASK) as u32;
        let mut n = 0u64;
        for &(first, end) in runs {
            for pg in first..end {
                if let Some(i) = info(pg as usize) {
                    i.ready.fetch_max(frame, Ordering::AcqRel);
                    n += 1;
                }
            }
        }
        SHARED.early_calls.fetch_add(1, Ordering::Relaxed);
        SHARED.early_pages.fetch_add(n, Ordering::Relaxed);
        wake_all();
    }

    /// The GPU finished frame `frame` (and, by the order of the queue, every frame before it). Its pages that are still
    /// protected for it go to clean and read only. It runs on the thread of the Metal completion handler.
    pub fn complete(&self, frame: u64) {
        SHARED.completed.fetch_max(frame, Ordering::AcqRel);
        if SHARED.mirror.load(Ordering::Acquire) != 0 {
            // With a mirror the pages stay tagged: the guest bytes are stale until a CPU access copies them. Pages that
            // a host thread opened for reading while the frame ran lose their access again.
            let (due, mut due_masks): (Vec<FrameRuns>, Vec<FrameMasks>) = {
                let mut c = ctrl();
                let mut due = Vec::new();
                while c.frames.front().is_some_and(|(f, _)| *f <= frame) {
                    if let Some(x) = c.frames.pop_front() {
                        due.push(x);
                    }
                }
                let mut masks = Vec::new();
                while c.masks.front().is_some_and(|(f, _)| *f <= frame) {
                    if let Some(x) = c.masks.pop_front() {
                        masks.push(x);
                    }
                }
                (due, masks)
            };
            let eager = SHARED.eager.load(Ordering::Acquire);
            let mirror = SHARED.mirror.load(Ordering::Acquire);
            let copy_back = SHARED.eager_on.load(Ordering::Acquire) && SHARED.alias.load(Ordering::Acquire) != 0;
            let mut later: Vec<FrameRuns> = Vec::new();
            let alias = SHARED.alias.load(Ordering::Acquire);
            if SHARED.late.load(Ordering::Acquire) && alias != 0 {
                // Under the control mutex, so that a flush that tags a page again is either before it (the page is
                // copied here) or after it (the flush copies a page of a complete frame itself).
                let _c = ctrl();
                for (f, runs) in &due {
                    let m = due_masks.iter().position(|(g, _)| g == f).map(|pos| due_masks.swap_remove(pos).1);
                    late_copy_back(*f, runs, m.as_deref(), mirror, alias);
                }
            }
            for (f, runs) in due {
                close_opened(f, &runs);
                if copy_back {
                    later.push((f, runs.clone()));
                }
                if eager {
                    for &(first, end) in &runs {
                        for pg in first..end {
                            let Some(w) = word(pg as usize) else { continue };
                            let cur = w.load(Ordering::Acquire);
                            if cur & STATE_MASK == GPU && u64::from(cur >> FRAME_SHIFT) == f & FRAME_MASK {
                                serve_mirrored(pg as usize, w, false, true, mirror, 0);
                            }
                        }
                    }
                }
            }
            wake_all();
            if !later.is_empty() {
                eager_submit(later);
            }
            return;
        }
        let due: Vec<(u64, Vec<(u32, u32)>)> = {
            let mut c = ctrl();
            let mut due = Vec::new();
            while c.frames.front().is_some_and(|(f, _)| *f <= frame) {
                if let Some(x) = c.frames.pop_front() {
                    due.push(x);
                }
            }
            due
        };
        for (f, runs) in due {
            release_runs(f, &runs);
        }
    }

    /// The late staging of `NXKIT_PRIVATE=1` with the knob `privlatestage`: the pages that the CPU wrote since the flush of
    /// the frame go back to read only and `stager` takes their bytes again. Returns the pages.
    pub fn flush_late(&self, stager: &mut dyn Stager) -> Vec<u32> {
        if !self.attached() || SHARED.mirror.load(Ordering::Acquire) == 0 {
            return Vec::new();
        }
        let _c = ctrl();
        let p = page();
        let base = SHARED.base.load(Ordering::Acquire);
        let n = SHARED.log_n.swap(0, Ordering::AcqRel).min(LOG_CAP);
        let mut dirty: Vec<u32> = Vec::new();
        for slot in &SHARED.log[..n] {
            let mut spins = 0;
            let v = loop {
                let v = slot.swap(0, Ordering::AcqRel);
                if v != 0 || spins > 10_000 {
                    break v;
                }
                spins += 1;
                std::hint::spin_loop();
            };
            if v != 0 {
                dirty.push(v - 1);
            }
        }
        dirty.sort_unstable();
        dirty.dedup();
        let emu: Vec<u32> = dirty
            .iter()
            .copied()
            .filter(|&pg| info(pg as usize).is_some_and(|i| i.emu.load(Ordering::Acquire) & 1 != 0))
            .collect();
        let mut back: Vec<u32> = Vec::new();
        for pg in dirty {
            if word(pg as usize)
                .is_some_and(|w| w.compare_exchange(DIRTY, CLEAN, Ordering::AcqRel, Ordering::Acquire).is_ok())
            {
                back.push(pg);
            }
        }
        stager.begin(back.len() + emu.len());
        for (s, e) in runs_of(&back) {
            protect_pages(s, e, PROT_READ, true);
            for pg in s..e {
                // SAFETY: the page lies in guest memory and is readable since the protection above.
                let bytes = unsafe {
                    std::slice::from_raw_parts((base + u64::from(pg) * p) as *const u8, p as usize)
                };
                stager.stage_masked(pg, bytes, stage_skip(pg as usize));
            }
        }
        if self.stage_emulated(&emu, &back, stager) > 0 {
            let extra: Vec<u32> = emu.iter().copied().filter(|pg| back.binary_search(pg).is_err()).collect();
            back.extend(extra);
            back.sort_unstable();
        }
        back
    }

    /// Stages the pages of `emu` (page numbers, sorted) that the handler stored to, except the ones in `done` (sorted),
    /// which were staged whole. The bytes come through the alias, because a page of a frame in flight has no access; the
    /// stale blocks go along with a mask and are not copied into the mirror. The flag is taken first: a store after it
    /// sets it again and logs the page for the next flush. Returns the pages staged.
    fn stage_emulated(&self, emu: &[u32], done: &[u32], stager: &mut dyn Stager) -> u32 {
        let mut n = 0;
        for &pg in emu {
            let Some(i) = info(pg as usize) else { continue };
            if i.emu.swap(0, Ordering::AcqRel) & 1 == 0 || done.binary_search(&pg).is_ok() {
                continue;
            }
            if word(pg as usize).is_none_or(|w| w.load(Ordering::Acquire) & STATE_MASK == FREE) {
                continue;
            }
            let Some(bytes) = self.alias_page(pg) else {
                continue;
            };
            stager.stage_masked(pg, bytes, i.take_skip());
            n += 1;
        }
        SHARED.emu_staged.fetch_add(u64::from(n), Ordering::Relaxed);
        n
    }

    /// The host reads or writes the range through a system call, and there is a mirror: the pages in the range that
    /// are tagged with a frame are brought up to date (the caller waits for the writer, an internal thread does not),
    /// so that the system call finds current bytes. A write leaves them dirty.
    fn settle(&self, addr: u64, len: u64, write: bool) {
        let Some((first, end)) = page_span(addr, len) else {
            return;
        };
        let mirror = SHARED.mirror.load(Ordering::Acquire);
        let internal = is_internal();
        for p in first..end {
            let Some(w) = word(p as usize) else { break };
            for _ in 0..64 {
                let state = w.load(Ordering::Acquire) & STATE_MASK;
                match state {
                    GPU => {
                        if !write && !internal && late_host_read(p as usize, w, addr, len) {
                            // A read outside the bytes the frame writes, of a page whose guest bytes are a completed
                            // frame's: the read needs no wait (the caller opens the page for reading next).
                            SHARED.settle_late.fetch_add(1, Ordering::Relaxed);
                            break;
                        }
                        let k = serve_mirrored(p as usize, w, write, internal, mirror, addr);
                        if matches!(k, FaultKind::ReadInFlight | FaultKind::WriteInFlight) {
                            SHARED.settle_waits.fetch_add(1, Ordering::Relaxed);
                        }
                        SHARED.faults[k as usize][FaultOrigin::Host as usize].fetch_add(1, Ordering::Relaxed);
                    }
                    OPEN if write => {
                        let k = serve_mirrored(p as usize, w, write, internal, mirror, addr);
                        SHARED.faults[k as usize][FaultOrigin::Host as usize]
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    SYNC => sleep_on_wake(SHARED.wake.load(Ordering::Acquire)),
                    _ => break,
                }
            }
        }
    }

    /// The flush of frame `frame` when there is a mirror ([`set_mirror`](Self::set_mirror)); the mirror of every page
    /// in `members` must be readable and writable.
    ///
    /// 1. The pages of `members` (host address and length) that are not tracked yet become tracked and read only, and
    ///    their guest bytes are copied into the mirror (the protection first, the copy second: a store after the
    ///    protection faults and is staged below or at the next flush).
    /// 2. The pages that the CPU dirtied since the last flush go back to clean and read only, and `stager` takes their
    ///    bytes (protection first, copy second).
    /// 3. The pages of `written` are tagged with the frame and lose all access. A page that was staged is tagged fresh.
    ///    With `poison` their guest bytes are first overwritten with the pattern, so that a read of guest memory that
    ///    should have gone to the mirror shows.
    ///
    /// The completion of the frame ([`complete`](Self::complete)) leaves the pages tagged: guest memory is stale until a
    /// CPU access copies the page from the mirror.
    pub fn flush_mirrored(
        &self,
        frame: u64,
        members: &[(u64, u64)],
        written: &[(u64, u64)],
        poison: Option<u32>,
        stager: &mut dyn Stager,
    ) -> FlushStats {
        let mut st = FlushStats::default();
        let mirror = SHARED.mirror.load(Ordering::Acquire);
        if !self.attached() || mirror == 0 {
            return st;
        }
        let t0 = now_ns();
        let mut lap = t0;
        let mut tick = || {
            let now = now_ns();
            let d = now.saturating_sub(lap);
            lap = now;
            d
        };
        let calls0 = SHARED.mprotects.load(Ordering::Relaxed);
        let prot0 = SHARED.mprotect_ns.load(Ordering::Relaxed);
        let mut c = ctrl();
        let cc = &mut *c;
        SHARED.epoch.store((frame / EPOCH_FRAMES) as u32, Ordering::Relaxed);
        let p = page();
        let base = SHARED.base.load(Ordering::Acquire);
        // From here until the commit of the frame, first writes of the CPU are counted and logged for it.
        SHARED.p1_n[frame as usize % P1_RING].store(0, Ordering::Release);
        SHARED.flushed.store(frame, Ordering::Release);
        let runs_w = span_runs(written);
        let mut all: Vec<(u64, u64)> = members.to_vec();
        all.extend_from_slice(written);
        let runs_m = span_runs(&all);
        // 1. New members.
        let mut newly: Vec<u32> = Vec::new();
        for &(s, e) in &runs_m {
            for pg in s..e {
                if let Some(w) = word(pg as usize)
                    && w.load(Ordering::Relaxed) & STATE_MASK == FREE
                    && w.compare_exchange(FREE, CLEAN, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                {
                    newly.push(pg);
                    member_bit(pg as usize, true);
                }
            }
        }
        for (s, e) in runs_of(&newly) {
            protect_pages(s, e, PROT_READ, true);
            // SAFETY: both ranges lie inside guest memory and its mirror (page numbers are clipped to the table); the
            // guest pages are readable (just protected read only), the mirror pages are writable (the caller made them
            // usable) and the two are different mappings.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    (base + u64::from(s) * p) as *const u8,
                    (mirror + u64::from(s) * p) as *mut u8,
                    ((e - s) as u64 * p) as usize,
                );
            }
        }
        SHARED.members.fetch_add(newly.len() as u64, Ordering::AcqRel);
        st.new_members = newly.len() as u32;
        st.new_ns = tick();
        // 2. The dirty pages.
        let n = SHARED.log_n.swap(0, Ordering::AcqRel);
        let mut dirty: Vec<u32> = Vec::new();
        if n > LOG_CAP {
            st.scanned = true;
            let total = SHARED.npages.load(Ordering::Acquire);
            for pg in 0..total {
                if word(pg).is_some_and(|w| w.load(Ordering::Relaxed) == DIRTY) {
                    dirty.push(pg as u32);
                }
            }
        } else {
            for slot in &SHARED.log[..n] {
                let mut spins = 0;
                let v = loop {
                    let v = slot.swap(0, Ordering::AcqRel);
                    if v != 0 || spins > 10_000 {
                        break v;
                    }
                    spins += 1;
                    std::hint::spin_loop();
                };
                if v != 0 {
                    dirty.push(v - 1);
                }
            }
            dirty.sort_unstable();
            dirty.dedup();
        }
        // The pages the handler stored to without opening them.
        let mut emu: Vec<u32> = dirty
            .iter()
            .copied()
            .filter(|&pg| info(pg as usize).is_some_and(|i| i.emu.load(Ordering::Acquire) & 1 != 0))
            .collect();
        if st.scanned {
            for pg in 0..SHARED.npages.load(Ordering::Acquire) {
                if word(pg).is_some_and(|w| w.load(Ordering::Relaxed) & STATE_MASK != FREE)
                    && info(pg).is_some_and(|i| i.emu.load(Ordering::Acquire) & 1 != 0)
                {
                    emu.push(pg as u32);
                }
            }
            emu.sort_unstable();
            emu.dedup();
        }
        st.log_ns = tick();
        stager.begin(dirty.len() + emu.len() + cc.hot.len() + 64);
        // The pages the CPU dirtied in several flushes in a row stay writable (step 5).
        let hot_on = SHARED.hot_on.load(Ordering::Acquire);
        let mut to_hot: Vec<u32> = Vec::new();
        if hot_on {
            for &pg in &cc.prev_dirty {
                if dirty.binary_search(&pg).is_err()
                    && let Some(x) = cc.streak.get_mut(pg as usize)
                {
                    *x = 0;
                }
            }
            for &pg in &dirty {
                if let Some(x) = cc.streak.get_mut(pg as usize) {
                    *x = x.saturating_add(1);
                    if *x >= HOT_AFTER && !in_runs(&runs_w, pg) && !cc.hot.contains_key(&pg) {
                        to_hot.push(pg);
                    }
                }
            }
            cc.prev_dirty.clone_from(&dirty);
        }
        let mut back: Vec<u32> = Vec::with_capacity(dirty.len());
        for pg in dirty {
            if cc.hot.contains_key(&pg) || to_hot.binary_search(&pg).is_ok() {
                continue;
            }
            if word(pg as usize).is_some_and(|w| {
                w.compare_exchange(DIRTY, CLEAN, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            }) {
                back.push(pg);
            }
        }
        st.dirtied = back.len() as u32;
        let stage_run = |stager: &mut dyn Stager, s: u32, e: u32| {
            for pg in s..e {
                // SAFETY: the page lies in guest memory and is readable (read only or open) since the protection above.
                let bytes = unsafe {
                    std::slice::from_raw_parts((base + u64::from(pg) * p) as *const u8, p as usize)
                };
                stager.stage_masked(pg, bytes, stage_skip(pg as usize));
            }
        };
        for (s, e) in runs_of(&back) {
            protect_pages(s, e, PROT_READ, true);
            stage_run(stager, s, e);
        }
        st.staged = back.len() as u32;
        st.emu_staged = self.stage_emulated(&emu, &back, stager);
        st.staged += st.emu_staged;
        st.stage_ns = tick();
        // The writable pages: each is compared with the bytes staged for it last, and staged when it differs. A page that
        // did not change for [`HOT_QUIET`] flushes is protected again; the compare after the protection catches a store
        // that came in between.
        st.promoted = to_hot.len() as u32;
        for pg in to_hot {
            cc.hot.insert(pg, Hot { shadow: Vec::new(), quiet: 0 });
        }
        if !cc.hot.is_empty() {
            let mut gone: Vec<u32> = Vec::new();
            let mut quiet: Vec<u32> = Vec::new();
            for (&pg, h) in cc.hot.iter_mut() {
                let still = word(pg as usize).is_some_and(|w| w.load(Ordering::Acquire) & STATE_MASK == DIRTY);
                if !still || in_runs(&runs_w, pg) {
                    // A frame writes it (step 3 stages it and tags it) or it is not tracked any more.
                    gone.push(pg);
                    continue;
                }
                // SAFETY: the page lies in guest memory and is readable and writable (it is dirty); the CPU may store
                // to it at the same time, which the staging accepts: the shadow is what is staged, so a later store is
                // found at the next flush.
                let live = unsafe { std::slice::from_raw_parts((base + u64::from(pg) * p) as *const u8, p as usize) };
                if h.shadow.len() == p as usize && h.shadow[..] == *live {
                    st.hot_same += 1;
                    h.quiet += 1;
                    if h.quiet >= HOT_QUIET {
                        quiet.push(pg);
                    }
                    continue;
                }
                h.quiet = 0;
                h.shadow.resize(p as usize, 0);
                h.shadow.copy_from_slice(live);
                stager.stage_masked(pg, &h.shadow, stage_skip(pg as usize));
                st.hot_staged += 1;
            }
            for pg in &gone {
                cc.hot.remove(pg);
                if let Some(x) = cc.streak.get_mut(*pg as usize) {
                    *x = 0;
                }
            }
            let mut demote: Vec<(u32, Vec<u8>)> = Vec::new();
            for pg in quiet {
                if let Some(h) = cc.hot.remove(&pg)
                    && word(pg as usize)
                        .is_some_and(|w| w.compare_exchange(DIRTY, CLEAN, Ordering::AcqRel, Ordering::Acquire).is_ok())
                {
                    demote.push((pg, h.shadow));
                }
                if let Some(x) = cc.streak.get_mut(pg as usize) {
                    *x = 0;
                }
            }
            let pgs: Vec<u32> = demote.iter().map(|d| d.0).collect();
            for (s, e) in runs_of(&pgs) {
                protect_pages(s, e, PROT_READ, true);
            }
            for (pg, shadow) in &demote {
                // SAFETY: as above; the page is read only now, so nothing changes under the compare.
                let live = unsafe { std::slice::from_raw_parts((base + u64::from(*pg) * p) as *const u8, p as usize) };
                if shadow[..] != *live {
                    stager.stage_masked(*pg, live, stage_skip(*pg as usize));
                    st.hot_staged += 1;
                }
            }
            st.demoted = demote.len() as u32;
            st.staged += st.hot_staged;
        }
        st.hot = cc.hot.len() as u32;
        st.hot_ns = tick();
        // 3. The pages of the frame.
        let masks = range_masks(&runs_w, written);
        let late_on = SHARED.late.load(Ordering::Acquire);
        let alias = SHARED.alias.load(Ordering::Acquire);
        for (ri, &(s, e)) in runs_w.iter().enumerate() {
            for pg in s..e {
                let Some(w) = word(pg as usize) else { continue };
                if late_on && alias != 0 {
                    // A page that a complete frame still tags and that game threads read late: its guest bytes are the
                    // last completed frame's only after the copy, which the copy-back thread may not have done yet.
                    let cur = w.load(Ordering::Acquire);
                    if cur & STATE_MASK == GPU
                        && !in_flight(cur >> FRAME_SHIFT)
                        && let Some(i) = info(pg as usize)
                        && i.snap.load(Ordering::Acquire) != 0
                    {
                        if late_active(i) {
                            let f = cur >> FRAME_SHIFT;
                            copy_stale(pg as usize, mirror, alias, false, f);
                            for a in &i.stale {
                                a.store(0, Ordering::Release);
                            }
                            i.snap.store(f.max(1), Ordering::Release);
                            SHARED.late_flush_copies.fetch_add(1, Ordering::Relaxed);
                        } else {
                            i.snap_reset(SnapWhy::FlushIdle);
                        }
                    }
                }
                let mut fresh = back.binary_search(&pg).is_ok();
                let mut tries = 0;
                loop {
                    let cur = w.load(Ordering::Acquire);
                    match cur & STATE_MASK {
                        DIRTY => {
                            // Written after step 2: take it back to clean and stage it again.
                            if w.compare_exchange(DIRTY, CLEAN, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                                protect_pages(pg, pg + 1, PROT_READ, true);
                                stage_run(stager, pg, pg + 1);
                                fresh = true;
                                st.raced += 1;
                                st.staged += 1;
                            }
                        }
                        SYNC if tries < 1000 => {
                            // A thread copies the page right now; let it finish.
                            tries += 1;
                            std::thread::yield_now();
                        }
                        _ => {
                            let tag = pack(GPU, frame) | if fresh { FRESH } else { 0 };
                            if w.compare_exchange(cur, tag, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                                break;
                            }
                        }
                    }
                }
                // The blocks the frame may write are stale from here on, in guest memory, until a copy from the
                // mirror (the tag came first: a thread that copies the page waits for the frame, which is not done).
                if let Some(i) = info(pg as usize) {
                    i.or_stale(&masks[ri][(pg - s) as usize]);
                }
            }
            // The pages lose all access first: from here a thread that touches a page faults, finds it tagged and
            // waits for the frame and copies the mirror. Only then is the pattern written, one page at a time, so a page
            // is open and holds the pattern for a few microseconds, not for the time of the whole run.
            //
            // Results one frame late: a page whose guest bytes are a completed frame's (`snap`) and that the completions
            // keep current stays readable. A read of it needs no trap: it gets the last completed frame's bytes, which is
            // what the handler served after the trap. Only stores fault (the handler marks the blocks as the CPU's).
            if late_on && alias != 0 && poison.is_none() && SHARED.read_open_on.load(Ordering::Acquire) {
                let readable = |pg: u32| info(pg as usize).is_some_and(|i| i.snap.load(Ordering::Acquire) != 0);
                let mut run_start = s;
                while run_start < e {
                    let open = readable(run_start);
                    let mut run_end = run_start + 1;
                    while run_end < e && readable(run_end) == open {
                        run_end += 1;
                    }
                    protect_pages(run_start, run_end, if open { PROT_READ } else { PROT_NONE }, true);
                    if open {
                        SHARED.read_open.fetch_add(u64::from(run_end - run_start), Ordering::Relaxed);
                    }
                    run_start = run_end;
                }
            } else {
                protect_pages(s, e, PROT_NONE, true);
            }
            if let Some(pattern) = poison {
                for pg in s..e {
                    // The pattern replaces the bytes of the last completed frame: late readers must wait here.
                    if let Some(i) = info(pg as usize) {
                        i.snap_reset(SnapWhy::Poison);
                    }
                    let stale = info(pg as usize).map_or([u64::MAX; 4], PageInfo::stale);
                    protect_pages(pg, pg + 1, PROT_RW, true);
                    for b in 0..(p / BLOCK) {
                        if stale[(b / 64) as usize] >> (b % 64) & 1 == 0 {
                            continue;
                        }
                        // SAFETY: the page is writable now and lies in guest memory; the block lies in the page.
                        unsafe {
                            std::slice::from_raw_parts_mut(
                                (base + u64::from(pg) * p + b * BLOCK) as *mut u32,
                                (BLOCK / 4) as usize,
                            )
                            .fill(pattern);
                        }
                    }
                    protect_pages(pg, pg + 1, PROT_NONE, true);
                }
            }
            st.written += e - s;
        }
        st.written_runs = runs_w.len() as u32;
        if !runs_w.is_empty() {
            if late_on {
                cc.masks.push_back((frame, masks));
            }
            cc.frames.push_back((frame, runs_w));
        }
        while cc.frames.len() > MAX_FRAMES {
            cc.frames.pop_front();
        }
        while cc.masks.len() > MAX_FRAMES {
            cc.masks.pop_front();
        }
        st.tag_ns = tick();
        st.mprotect_ns = SHARED.mprotect_ns.load(Ordering::Relaxed).saturating_sub(prot0);
        st.mprotects = (SHARED.mprotects.load(Ordering::Relaxed) - calls0) as u32;
        st.ns = now_ns().saturating_sub(t0);
        st
    }

    /// The host reads or writes the range through a system call. A tracked page that a read needs and that is protected
    /// against all access opens for reading; a write dirties the tracked pages and opens them for writing.
    fn open(&self, addr: u64, len: u64, write: bool) {
        let Some((first, end)) = page_span(addr, len) else {
            return;
        };
        let prot = if write { PROT_RW } else { PROT_READ };
        let mut start: Option<u32> = None;
        for p in first..end {
            let Some(w) = word(p as usize) else { break };
            let mut changed = false;
            loop {
                let cur = w.load(Ordering::Acquire);
                let state = cur & STATE_MASK;
                if write && (state == CLEAN || state == GPU || state == OPEN) {
                    if state == GPU && in_flight(cur >> FRAME_SHIFT) {
                        SHARED.host_access[1].fetch_add(1, Ordering::Relaxed);
                    }
                    if w.compare_exchange(cur, DIRTY, Ordering::AcqRel, Ordering::Acquire)
                        .is_err()
                    {
                        continue;
                    }
                    log_dirty(p as usize);
                    SHARED.hook_dirtied.fetch_add(1, Ordering::Relaxed);
                    changed = true;
                } else if !write && state == GPU {
                    if in_flight(cur >> FRAME_SHIFT) {
                        SHARED.host_access[0].fetch_add(1, Ordering::Relaxed);
                    }
                    changed = true;
                }
                break;
            }
            match (changed, start) {
                (true, None) => start = Some(p),
                (false, Some(s)) => {
                    protect_pages(s, p, prot, true);
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(s) = start {
            protect_pages(s, end, prot, true);
        }
    }

    /// The pages in the range stop being tracked and become readable and writable: the host changes their permission or
    /// gives them back.
    fn forget(&self, addr: u64, len: u64) {
        let Some((first, end)) = page_span(addr, len) else {
            return;
        };
        let mut start: Option<u32> = None;
        for p in first..end {
            let member = word(p as usize)
                .is_some_and(|w| w.load(Ordering::Acquire) & STATE_MASK != FREE);
            match (member, start) {
                (true, None) => start = Some(p),
                (false, Some(s)) => {
                    protect_pages(s, p, PROT_RW, true);
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(s) = start {
            protect_pages(s, end, PROT_RW, true);
        }
        // Open first, free after: a thread that faulted meanwhile finds a tracked page and opens it.
        let mut freed = 0u64;
        for p in first..end {
            if let Some(w) = word(p as usize)
                && w.swap(FREE, Ordering::AcqRel) & STATE_MASK != FREE
            {
                freed += 1;
                member_bit(p as usize, false);
                if let Some(i) = info(p as usize) {
                    i.clear();
                }
            }
        }
        let members = SHARED.members.load(Ordering::Acquire);
        SHARED
            .members
            .fetch_sub(freed.min(members), Ordering::AcqRel);
    }

    /// The numbers since the process started.
    pub fn stats(&self) -> PageStats {
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        let mut faults = [[0u64; 3]; 5];
        for (k, row) in faults.iter_mut().enumerate() {
            for (o, v) in row.iter_mut().enumerate() {
                *v = g(&SHARED.faults[k][o]);
            }
        }
        let learned = {
            let l = LEARNED.lock().unwrap_or_else(PoisonError::into_inner);
            (l.pages.len() as u64, l.changes)
        };
        PageStats {
            members: g(&SHARED.members),
            faults,
            handler_ns: g(&SHARED.handler_ns),
            host_access: [g(&SHARED.host_access[0]), g(&SHARED.host_access[1])],
            hook_dirtied: g(&SHARED.hook_dirtied),
            mprotects: g(&SHARED.mprotects),
            mprotect_ns: g(&SHARED.mprotect_ns),
            mprotect_failed: g(&SHARED.mprotect_failed),
            log_overflows: g(&SHARED.log_overflows),
            pc_overflow: g(&SHARED.pc_overflow),
            last_signal: g(&SHARED.last_signal),
            synced: g(&SHARED.synced),
            writer_waits: g(&SHARED.writer_waits),
            writer_wait_ns: g(&SHARED.writer_wait_ns),
            internal_opens: g(&SHARED.internal_opens),
            wait_timeouts: g(&SHARED.wait_timeouts),
            phase_writes: [
                g(&SHARED.phase_writes[0]),
                g(&SHARED.phase_writes[1]),
                g(&SHARED.phase_writes[2]),
            ],
            waits: [g(&SHARED.wait_n[0]), g(&SHARED.wait_n[1])],
            wait_ns: [g(&SHARED.wait_ns[0]), g(&SHARED.wait_ns[1])],
            wait_uncommitted: [g(&SHARED.wait_uncommitted[0]), g(&SHARED.wait_uncommitted[1])],
            emulated: std::array::from_fn(|i| g(&SHARED.emu[i])),
            emulated_ns: g(&SHARED.emu_ns),
            fallbacks: std::array::from_fn(|i| g(&SHARED.fallback[i])),
            emu_staged: g(&SHARED.emu_staged),
            eager_pages: g(&SHARED.eager_pages),
            eager_runs: g(&SHARED.eager_runs),
            eager_ns: g(&SHARED.eager_ns),
            wait_ends: [g(&SHARED.wait_end[0]), g(&SHARED.wait_end[1])],
            wait_end_ns: [g(&SHARED.wait_end_ns[0]), g(&SHARED.wait_end_ns[1])],
            early_calls: g(&SHARED.early_calls),
            early_pages: g(&SHARED.early_pages),
            early_syncs: g(&SHARED.early_syncs),
            readers: learned.0,
            reader_changes: learned.1,
            late_reads: g(&SHARED.late_reads),
            late_writes: g(&SHARED.late_writes),
            late_age_sum: g(&SHARED.late_age_sum),
            late_age: std::array::from_fn(|i| g(&SHARED.late_age[i])),
            late_owned: g(&SHARED.late_owned),
            late_kept: g(&SHARED.late_kept),
            late_copied: g(&SHARED.late_copied),
            late_copied_blocks: g(&SHARED.late_copied_blocks),
            late_flush_copies: g(&SHARED.late_flush_copies),
            snap_resets: std::array::from_fn(|i| g(&SHARED.snap_resets[i])),
            snap_misses: std::array::from_fn(|i| g(&SHARED.snap_misses[i])),
            read_open: g(&SHARED.read_open),
            read_revoked: g(&SHARED.read_revoked),
            settle_late: g(&SHARED.settle_late),
            settle_waits: g(&SHARED.settle_waits),
        }
    }

    /// The instruction word at `pc`, a program counter of an access the tables counted, for the reports.
    pub fn insn_at(&self, pc: u64) -> u32 {
        if pc & 3 != 0 || pc == 0 {
            return 0;
        }
        // SAFETY: the program counter of a fault names an instruction that was fetched, so the word is readable (the
        // code of the game stays mapped while it runs).
        unsafe { (pc as *const u32).read() }
    }

    /// The longest wait for a writer since the last call, by access (0 read, 1 write), in ns.
    pub fn take_wait_max(&self) -> [u64; 2] {
        [
            SHARED.wait_max_ns[0].swap(0, Ordering::Relaxed),
            SHARED.wait_max_ns[1].swap(0, Ordering::Relaxed),
        ]
    }

    /// The `n` program counters that faulted most for `kind` (only the kinds with a table: a clean write, a read and a
    /// write in flight), most first, with their counts. The tables are emptied.
    pub fn take_top_pcs(&self, kind: FaultKind, n: usize) -> Vec<(u64, u64)> {
        let k = kind as usize;
        if k > 2 {
            return Vec::new();
        }
        self.take_top(k, n)
    }

    /// The `n` program counters that wrote member pages between a flush and the commit of its frame, most first. The table
    /// is emptied.
    pub fn take_top_between_pcs(&self, n: usize) -> Vec<(u64, u64)> {
        self.take_top(3, n)
    }

    /// The `n` program counters whose access to a page in flight was not emulated, most first. The table is emptied.
    pub fn take_top_fallback_pcs(&self, n: usize) -> Vec<(u64, u64)> {
        self.take_top(PC_FALLBACK, n)
    }

    fn take_top(&self, k: usize, n: usize) -> Vec<(u64, u64)> {
        let mut v: Vec<(u64, u64)> = Vec::new();
        for (key, c) in &SHARED.pcs[k] {
            let pc = key.load(Ordering::Relaxed);
            if pc != 0 {
                v.push((pc, c.swap(0, Ordering::Relaxed)));
                key.store(0, Ordering::Relaxed);
            }
        }
        v.sort_unstable_by_key(|&(pc, c)| (std::cmp::Reverse(c), pc));
        v.truncate(n);
        v
    }
}

/// The hook of `nxkit_abi::guest`: called before the host reads or writes guest memory in a way that a fault cannot
/// hold back (`write` says which), and before it changes the permission of guest pages or gives them back (`write`
/// and a zero length mean the latter, see the callers).
fn hook(addr: u64, len: u64, access: u32) {
    if SHARED.members.load(Ordering::Acquire) == 0 {
        return;
    }
    let mirrored = SHARED.mirror.load(Ordering::Acquire) != 0;
    match access {
        nxkit_abi::guest::ACCESS_READ => {
            if mirrored {
                PAGES.settle(addr, len, false);
            }
            PAGES.open(addr, len, false);
        }
        nxkit_abi::guest::ACCESS_WRITE => {
            if mirrored {
                PAGES.settle(addr, len, true);
            }
            PAGES.open(addr, len, true);
        }
        _ => {
            if mirrored {
                PAGES.settle(addr, len, false);
            }
            PAGES.forget(addr, len);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::{Layout, alloc_zeroed, dealloc};
    use std::sync::Mutex as StdMutex;

    /// One test at a time: the tracker is one per process.
    static TURN: StdMutex<()> = StdMutex::new(());
    const P: u64 = 0x4000;

    struct Mem {
        base: *mut u8,
        layout: Layout,
    }

    impl Mem {
        fn new(pages: u64) -> Self {
            let layout = Layout::from_size_align((pages * P) as usize, P as usize).unwrap();
            // SAFETY: the layout is not empty.
            let base = unsafe { alloc_zeroed(layout) };
            assert!(!base.is_null());
            Self { base, layout }
        }

        fn addr(&self, page: u64) -> u64 {
            self.base as u64 + page * P
        }

        fn store(&self, page: u64, off: u64, v: u8) {
            // SAFETY: inside the allocation.
            unsafe {
                self.base
                    .add((page * P + off) as usize)
                    .write_volatile(v);
            }
        }

        fn load(&self, page: u64, off: u64) -> u8 {
            // SAFETY: inside the allocation.
            unsafe { self.base.add((page * P + off) as usize).read_volatile() }
        }
    }

    impl Drop for Mem {
        fn drop(&mut self) {
            // SAFETY: the allocation came from `alloc_zeroed` with this layout; make it writable first.
            unsafe {
                mprotect(self.base.cast(), self.layout.size(), PROT_RW);
                dealloc(self.base, self.layout);
            }
        }
    }

    fn setup(pages: u64) -> (Mem, &'static Pages) {
        let m = Mem::new(pages);
        let pg = Pages::global();
        pg.attach(m.addr(0), pages * P);
        (m, pg)
    }

    #[test]
    fn a_write_to_a_clean_page_dirties_it_and_the_flush_cleans_it() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, pg) = setup(8);
        let s0 = pg.stats();
        // Page 2 becomes tracked as written by frame 1, completes, and is clean and read only.
        let st = pg.flush(1, &[(m.addr(2), 100)]);
        assert_eq!((st.written, st.new_members), (1, 1));
        pg.complete(1);
        assert_eq!(pg.state_of(m.addr(2)), "clean");
        assert_eq!(m.load(2, 0), 0, "a read of a clean page does not fault");
        m.store(2, 7, 9);
        assert_eq!(m.load(2, 7), 9, "the store ran again after the handler");
        assert_eq!(pg.state_of(m.addr(2)), "dirty");
        // A second store to the open page does not fault.
        m.store(2, 8, 1);
        let s1 = pg.stats();
        assert_eq!(
            s1.faults[FaultKind::CleanWrite as usize].iter().sum::<u64>()
                - s0.faults[FaultKind::CleanWrite as usize].iter().sum::<u64>(),
            1
        );
        // The flush takes it back to clean; the next store faults again.
        let st = pg.flush(2, &[]);
        assert_eq!(st.dirtied, 1);
        assert_eq!(pg.state_of(m.addr(2)), "clean");
        m.store(2, 9, 2);
        assert_eq!(pg.state_of(m.addr(2)), "dirty");
        assert_eq!(m.load(2, 7), 9);
        assert_eq!(m.load(2, 9), 2);
    }

    #[test]
    fn accesses_in_flight_are_counted_by_kind_and_the_data_stays() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, pg) = setup(8);
        m.store(3, 0, 5);
        m.store(4, 0, 6);
        let s0 = pg.stats();
        let st = pg.flush(10, &[(m.addr(3), 2 * P)]);
        assert_eq!(st.written, 2);
        assert_eq!(pg.state_of(m.addr(3)), "gpu(10)");
        // A read in flight: counted, the page opens for reading, the data is the guest's.
        assert_eq!(m.load(3, 0), 5);
        // A second read does not fault; a write does, as a write in flight.
        assert_eq!(m.load(3, 1), 0);
        m.store(3, 2, 8);
        // The other page: a write first.
        m.store(4, 3, 7);
        let s1 = pg.stats();
        let d = |k: FaultKind| {
            s1.faults[k as usize].iter().sum::<u64>() - s0.faults[k as usize].iter().sum::<u64>()
        };
        assert_eq!(d(FaultKind::ReadInFlight), 1);
        assert_eq!(d(FaultKind::WriteInFlight), 2);
        assert_eq!(pg.state_of(m.addr(3)), "dirty");
        assert_eq!(m.load(3, 2), 8);
        assert_eq!(m.load(4, 0), 6);
        // The completion leaves dirty pages alone and a later write faults as a plain dirtying only after a flush.
        pg.complete(10);
        assert_eq!(pg.state_of(m.addr(3)), "dirty");
        let _ = pg.flush(11, &[]);
        assert_eq!(pg.state_of(m.addr(3)), "clean");
        let tops = pg.take_top_pcs(FaultKind::WriteInFlight, 10);
        assert!(!tops.is_empty() && tops.iter().map(|t| t.1).sum::<u64>() >= 2);
    }

    #[test]
    fn a_page_read_in_flight_is_clean_after_the_completion() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, pg) = setup(4);
        let _ = pg.flush(5, &[(m.addr(1), 1)]);
        assert_eq!(m.load(1, 0), 0);
        assert_eq!(pg.state_of(m.addr(1)), "gpu(5)");
        pg.complete(5);
        assert_eq!(pg.state_of(m.addr(1)), "clean");
        // After the completion the page is protected read only again: a write faults as a clean write.
        let s0 = pg.stats();
        m.store(1, 0, 1);
        let s1 = pg.stats();
        assert_eq!(
            s1.faults[FaultKind::CleanWrite as usize].iter().sum::<u64>()
                - s0.faults[FaultKind::CleanWrite as usize].iter().sum::<u64>(),
            1
        );
    }

    #[test]
    fn a_newer_frame_keeps_the_page_when_the_older_one_completes() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, pg) = setup(4);
        let _ = pg.flush(1, &[(m.addr(0), 1)]);
        let _ = pg.flush(2, &[(m.addr(0), 1)]);
        pg.complete(1);
        assert_eq!(pg.state_of(m.addr(0)), "gpu(2)");
        pg.complete(2);
        assert_eq!(pg.state_of(m.addr(0)), "clean");
    }

    #[test]
    fn the_hook_opens_pages_for_system_calls_and_forgets_released_ones() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, pg) = setup(6);
        let _ = pg.flush(1, &[(m.addr(0), 3 * P)]);
        pg.complete(1);
        assert_eq!(pg.state_of(m.addr(1)), "clean");
        // A system call writes into pages 1 and 2: both go dirty and writable, page 0 stays clean.
        hook(m.addr(1), 2 * P, nxkit_abi::guest::ACCESS_WRITE);
        assert_eq!(pg.state_of(m.addr(0)), "clean");
        assert_eq!(pg.state_of(m.addr(1)), "dirty");
        assert_eq!(pg.state_of(m.addr(2)), "dirty");
        m.store(1, 0, 1);
        // The host gives the range back: the pages are free and open.
        hook(m.addr(0), 3 * P, nxkit_abi::guest::ACCESS_FORGET);
        assert_eq!(pg.state_of(m.addr(1)), "free");
        assert_eq!(pg.stats().members, 0);
        m.store(0, 0, 1);
    }

    #[test]
    fn a_syscall_read_of_a_page_in_flight_does_not_fail() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, pg) = setup(4);
        let _ = pg.flush(1, &[(m.addr(0), 2 * P)]);
        let before = pg.stats().host_access[0];
        hook(m.addr(0), 2 * P, nxkit_abi::guest::ACCESS_READ);
        assert_eq!(pg.stats().host_access[0] - before, 2);
        // The page is readable now, as a system call needs.
        assert_eq!(m.load(0, 0), 0);
        pg.complete(1);
    }

    #[test]
    fn many_threads_write_the_same_clean_page() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, pg) = setup(4);
        let _ = pg.flush(1, &[(m.addr(0), P)]);
        pg.complete(1);
        let base = m.base as usize;
        let hs: Vec<_> = (0..8u64)
            .map(|i| {
                std::thread::spawn(move || {
                    // SAFETY: inside the allocation of the test, which outlives the joined threads.
                    unsafe { (base as *mut u8).add(i as usize).write_volatile(i as u8 + 1) };
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        for i in 0..8 {
            assert_eq!(m.load(0, i), i as u8 + 1);
        }
        assert_eq!(pg.flush(2, &[]).dirtied, 1);
    }

    #[test]
    fn internal_threads_are_counted_apart() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, pg) = setup(4);
        let _ = pg.flush(1, &[(m.addr(0), P)]);
        let s0 = pg.stats();
        let base = m.base as usize;
        std::thread::spawn(move || {
            let _g = mark_internal_thread();
            // SAFETY: inside the allocation of the test, which outlives the joined thread.
            unsafe { (base as *mut u8).write_volatile(1) };
        })
        .join()
        .unwrap();
        let s1 = pg.stats();
        assert_eq!(
            s1.faults[FaultKind::WriteInFlight as usize][FaultOrigin::Internal as usize]
                - s0.faults[FaultKind::WriteInFlight as usize][FaultOrigin::Internal as usize],
            1
        );
        pg.complete(1);
    }

    #[test]
    fn a_file_read_into_a_protected_page_works_through_the_guest_slice() {
        use std::io::Read;
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, pg) = setup(4);
        nxkit_abi::guest::set_range(m.addr(0), 4 * P);
        let _ = pg.flush(1, &[(m.addr(1), P)]);
        assert_eq!(pg.state_of(m.addr(1)), "gpu(1)");
        let mut f = std::fs::File::open("/dev/zero").unwrap();
        // The page allows no access: the system call would fail with EFAULT without the hook of the slice.
        m.store(1, 5, 0);
        let _ = pg.flush(2, &[(m.addr(1), P)]);
        let r = nxkit_abi::guest::with_slice_mut(m.addr(1), 64, |b| {
            f.read(b).map(|n| (n, b[0]))
        });
        assert_eq!(r.unwrap().unwrap(), (64, 0));
        assert_eq!(pg.state_of(m.addr(1)), "dirty");
        // A read through a system call (a write to a file) opens the page for reading too.
        let _ = pg.flush(3, &[(m.addr(1), P)]);
        let out = std::env::temp_dir().join(format!("nxkit-pages-{}", std::process::id()));
        let mut o = std::fs::File::create(&out).unwrap();
        let w = nxkit_abi::guest::with_slice(m.addr(1), 16, |b| {
            use std::io::Write;
            o.write(b)
        });
        let _ = std::fs::remove_file(&out);
        assert_eq!(w.unwrap().unwrap(), 16);
        nxkit_abi::guest::set_range(0, 0);
        pg.complete(3);
    }

    /// Collects the staged pages of a test flush.
    #[derive(Default)]
    struct Collect {
        pages: Vec<(u32, Vec<u8>)>,
        skips: Vec<(u32, [u64; 4])>,
    }

    impl Stager for Collect {
        fn begin(&mut self, _max: usize) {}

        fn stage(&mut self, page: u32, bytes: &[u8]) {
            self.stage_masked(page, bytes, [0; 4]);
        }

        fn stage_masked(&mut self, page: u32, bytes: &[u8], skip: [u64; 4]) {
            self.pages.retain(|p| p.0 != page);
            self.skips.retain(|p| p.0 != page);
            self.pages.push((page, bytes.to_vec()));
            self.skips.push((page, skip));
        }
    }

    fn mirrored(pages: u64) -> (Mem, Mem, &'static Pages) {
        let (m, pg) = setup(pages);
        let mir = Mem::new(pages);
        pg.set_mirror(mir.addr(0));
        (m, mir, pg)
    }

    #[test]
    fn a_new_member_is_copied_into_the_mirror_and_a_gpu_written_page_is_copied_back_on_a_read() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, mir, pg) = mirrored(8);
        m.store(2, 5, 42);
        let mut st = Collect::default();
        let f = pg.flush_mirrored(1, &[(m.addr(2), 8)], &[(m.addr(2), 8)], None, &mut st);
        assert_eq!((f.new_members, f.written, f.staged), (1, 1, 0));
        assert_eq!(mir.load(2, 5), 42, "the guest bytes went into the mirror");
        assert_eq!(pg.state_of(m.addr(2)), "gpu(1)");
        // The GPU writes the mirror; the guest page is stale and has no access.
        mir.store(2, 5, 77);
        mir.store(2, 6, 78);
        // The frame completes; a read copies the page.
        pg.complete(1);
        assert_eq!(pg.state_of(m.addr(2)), "gpu(1)", "the completion leaves the page tagged");
        let s0 = pg.stats();
        assert_eq!(m.load(2, 5), 77);
        assert_eq!(m.load(2, 6), 78);
        assert_eq!(pg.state_of(m.addr(2)), "clean");
        let s1 = pg.stats();
        assert_eq!(s1.synced - s0.synced, 1);
        assert_eq!(s1.writer_waits, s0.writer_waits, "no wait: the writer was complete");
        // A write now dirties it; the flush stages the whole page and tags it fresh when a frame writes it.
        m.store(2, 7, 9);
        assert_eq!(pg.state_of(m.addr(2)), "dirty");
        let f = pg.flush_mirrored(2, &[], &[(m.addr(2), 8)], None, &mut st);
        assert_eq!((f.staged, f.written), (1, 1));
        assert_eq!(st.pages.len(), 1);
        assert_eq!(st.pages[0].1[5], 77, "the page holds the GPU's bytes and the CPU's");
        assert_eq!(st.pages[0].1[7], 9);
        assert_eq!(pg.state_of(m.addr(2)), "gpu(2, fresh)");
        pg.complete(2);
        pg.set_mirror(0);
    }

    #[test]
    fn a_read_of_a_page_in_flight_waits_for_the_completion() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, mir, pg) = mirrored(4);
        let mut st = Collect::default();
        let _ = pg.flush_mirrored(5, &[], &[(m.addr(1), 1)], None, &mut st);
        mir.store(1, 0, 11);
        let s0 = pg.stats();
        let base = m.base as usize;
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let d2 = done.clone();
        let reader = std::thread::spawn(move || {
            // SAFETY: inside the allocation of the test, which outlives the joined thread.
            let v = unsafe { (base as *const u8).add(P as usize).read_volatile() };
            d2.store(true, Ordering::Release);
            v
        });
        std::thread::sleep(std::time::Duration::from_millis(80));
        assert!(!done.load(Ordering::Acquire), "the reader waits for the writer");
        // The GPU finishes: it wrote the mirror before the completion.
        mir.store(1, 0, 12);
        pg.complete(5);
        assert_eq!(reader.join().unwrap(), 12);
        let s1 = pg.stats();
        assert_eq!(s1.writer_waits - s0.writer_waits, 1);
        assert!(s1.writer_wait_ns > s0.writer_wait_ns);
        pg.set_mirror(0);
    }

    #[test]
    fn a_reader_goes_on_at_the_pass_signal_of_its_page_while_the_frame_runs_and_the_other_page_waits_for_the_frame() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, mir, pg) = mirrored(6);
        let mut st = Collect::default();
        let _ = pg.flush_mirrored(31, &[], &[(m.addr(1), 1), (m.addr(2), 1)], None, &mut st);
        mir.store(1, 0, 11);
        mir.store(2, 0, 21);
        let s0 = pg.stats();
        let base = m.base as usize;
        let read = move |page: usize| {
            std::thread::spawn(move || {
                // SAFETY: inside the allocation of the test, which outlives the joined thread.
                unsafe { (base as *const u8).add(page * P as usize).read_volatile() }
            })
        };
        let (r1, r2) = (read(1), read(2));
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert!(!r1.is_finished() && !r2.is_finished(), "both wait for their writers");
        // The pass that writes page 1 is done; the frame is not.
        pg.release_early(31, &[(1, 2)]);
        assert_eq!(r1.join().unwrap(), 11, "the bytes of the pass");
        std::thread::sleep(std::time::Duration::from_millis(40));
        assert!(!r2.is_finished(), "page 2 is written by a later pass");
        assert_eq!(pg.completed(), 0, "the frame did not complete");
        assert_eq!(pg.state_of(m.addr(1)), "clean");
        mir.store(2, 0, 22);
        pg.complete(31);
        assert_eq!(r2.join().unwrap(), 22);
        let s1 = pg.stats();
        assert_eq!(s1.wait_ends[0] - s0.wait_ends[0], 1, "one wait ended at a pass signal");
        assert_eq!(s1.wait_ends[1] - s0.wait_ends[1], 1, "one at the completion");
        assert_eq!(s1.early_pages - s0.early_pages, 1);
        assert_eq!(s1.early_calls - s0.early_calls, 1);
        pg.set_mirror(0);
    }

    #[test]
    fn a_page_a_reader_waited_for_is_learned_for_a_while_and_forgotten() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, mir, pg) = mirrored(6);
        let mut st = Collect::default();
        let _ = pg.readers(0);
        let _ = pg.flush_mirrored(40, &[], &[(m.addr(3), 1)], None, &mut st);
        mir.store(3, 0, 5);
        let base = m.base as usize;
        let reader = std::thread::spawn(move || {
            // SAFETY: inside the allocation of the test, which outlives the joined thread.
            unsafe { (base as *const u8).add(3 * P as usize).read_volatile() }
        });
        std::thread::sleep(std::time::Duration::from_millis(40));
        pg.complete(40);
        assert_eq!(reader.join().unwrap(), 5);
        let c0 = pg.stats().reader_changes;
        assert_eq!(pg.readers(41), vec![3], "the page waited for the writer");
        assert_eq!(pg.stats().reader_changes - c0, 1, "the set changed once");
        assert_eq!(pg.readers(42), vec![3], "it stays");
        assert_eq!(pg.stats().reader_changes - c0, 1);
        assert!(pg.readers(40 + LEARN_FRAMES + 5).is_empty(), "and decays");
        assert_eq!(pg.stats().reader_changes - c0, 2);
        pg.set_mirror(0);
    }

    #[test]
    fn waits_for_a_writer_are_counted_by_access_with_their_time_and_whether_the_frame_was_committed() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, mir, pg) = mirrored(4);
        let mut st = Collect::default();
        // Frame 21 is flushed and not committed; it may write pages 1 and 2.
        let _ = pg.flush_mirrored(21, &[], &[(m.addr(1), 1), (m.addr(2), 1)], None, &mut st);
        let _ = (pg.take_wait_max(), pg.take_in_flight());
        let s0 = pg.stats();
        let base = m.base as usize;
        let reader = std::thread::spawn(move || {
            // SAFETY: inside the allocation of the test, which outlives the joined thread.
            unsafe { (base as *const u8).add(P as usize).read_volatile() }
        });
        std::thread::sleep(std::time::Duration::from_millis(40));
        // The commit: a wait that begins now begins after it.
        pg.note_committed(21);
        let writer = std::thread::spawn(move || {
            // SAFETY: as above.
            unsafe { (base as *mut u8).add(2 * P as usize).write_volatile(3) };
        });
        std::thread::sleep(std::time::Duration::from_millis(40));
        mir.store(1, 0, 9);
        pg.complete(21);
        assert_eq!(reader.join().unwrap(), 9);
        writer.join().unwrap();
        let s1 = pg.stats();
        assert_eq!((s1.waits[0] - s0.waits[0], s1.waits[1] - s0.waits[1]), (1, 1), "one read wait, one write wait");
        assert_eq!(
            (s1.wait_uncommitted[0] - s0.wait_uncommitted[0], s1.wait_uncommitted[1] - s0.wait_uncommitted[1]),
            (1, 0),
            "the read began before the commit, the write after it"
        );
        assert!(s1.wait_ns[0] - s0.wait_ns[0] >= 60_000_000, "the read waited for both sleeps");
        assert!(s1.wait_ns[1] - s0.wait_ns[1] >= 30_000_000);
        let longest = pg.take_wait_max();
        assert!(longest[0] >= longest[1] && longest[1] >= 30_000_000);
        let (log, total) = pg.take_in_flight();
        assert_eq!(total, 2);
        assert!(log.contains(&(21, m.addr(1), false)) && log.contains(&(21, m.addr(2), true)), "{log:?}");
        pg.set_mirror(0);
    }

    #[test]
    fn first_writes_after_a_flush_are_logged_for_the_frame_until_its_commit() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, _mir, pg) = mirrored(4);
        let mut st = Collect::default();
        let _ = pg.flush_mirrored(31, &[(m.addr(1), 1), (m.addr(3), 1)], &[], None, &mut st);
        m.store(1, 40, 1);
        // A second write to the same page is not a first write.
        m.store(1, 80, 1);
        pg.note_committed(31);
        m.store(3, 0, 1);
        let (log, total) = pg.take_between(31);
        assert_eq!((log, total), (vec![m.addr(1) + 40], 1), "only the write before the commit, at its address");
        assert_eq!(pg.take_between(31).1, 0, "the log is emptied");
        pg.set_mirror(0);
    }

    #[test]
    fn a_write_in_flight_waits_then_the_cpu_write_lands_on_the_gpus_bytes() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, mir, pg) = mirrored(4);
        let mut st = Collect::default();
        // The frame may write bytes 0 to 127 (a copy from the mirror moves the 64-byte blocks of the written ranges).
        let _ = pg.flush_mirrored(3, &[], &[(m.addr(0), 128)], None, &mut st);
        mir.store(0, 100, 5);
        let s0 = pg.stats();
        let base = m.base as usize;
        let writer = std::thread::spawn(move || {
            // SAFETY: inside the allocation of the test, which outlives the joined thread.
            unsafe { (base as *mut u8).add(101).write_volatile(6) };
        });
        std::thread::sleep(std::time::Duration::from_millis(60));
        mir.store(0, 100, 7);
        pg.complete(3);
        writer.join().unwrap();
        assert_eq!(m.load(0, 100), 7, "the GPU's byte is there");
        assert_eq!(m.load(0, 101), 6, "and the CPU's");
        assert_eq!(pg.state_of(m.addr(0)), "dirty");
        let s1 = pg.stats();
        let d = |k: FaultKind| {
            s1.faults[k as usize].iter().sum::<u64>() - s0.faults[k as usize].iter().sum::<u64>()
        };
        assert_eq!(d(FaultKind::WriteInFlight), 1);
        pg.set_mirror(0);
    }

    #[test]
    fn an_internal_thread_does_not_wait_and_the_page_loses_its_access_at_the_completion() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, mir, pg) = mirrored(4);
        let mut st = Collect::default();
        // A page that the frame writes and that the CPU had not written: the mirror is the truth.
        m.store(1, 0, 1);
        let _ = pg.flush_mirrored(9, &[(m.addr(1), 1)], &[(m.addr(1), 1)], None, &mut st);
        mir.store(1, 0, 2);
        let base = m.base as usize;
        let v = std::thread::spawn(move || {
            let _g = mark_internal_thread();
            // SAFETY: inside the allocation of the test, which outlives the joined thread.
            unsafe { (base as *const u8).add(P as usize).read_volatile() }
        })
        .join()
        .unwrap();
        assert_eq!(v, 2, "the internal thread got the mirror's bytes at once");
        assert_eq!(pg.state_of(m.addr(1)), "open(9)");
        assert!(pg.stats().internal_opens > 0);
        pg.complete(9);
        assert_eq!(pg.state_of(m.addr(1)), "gpu(9)");
        // The GPU wrote more; the game's read after the completion sees it.
        mir.store(1, 0, 3);
        assert_eq!(m.load(1, 0), 3);
        pg.set_mirror(0);
    }

    #[test]
    fn a_poisoned_page_holds_the_pattern_until_a_cpu_access_copies_the_mirror() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, mir, pg) = mirrored(4);
        let mut st = Collect::default();
        m.store(3, 0, 8);
        let _ = pg.flush_mirrored(1, &[], &[(m.addr(3), 1)], Some(0x7fc0_0000), &mut st);
        assert_eq!(mir.load(3, 0), 8);
        pg.complete(1);
        // The access copies the mirror over the pattern.
        assert_eq!(m.load(3, 0), 8);
        pg.set_mirror(0);
    }

    #[test]
    fn a_system_call_reading_a_gpu_written_page_finds_the_mirrors_bytes() {
        use std::io::Write;
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, mir, pg) = mirrored(4);
        nxkit_abi::guest::set_range(m.addr(0), 4 * P);
        let mut st = Collect::default();
        let _ = pg.flush_mirrored(1, &[], &[(m.addr(1), P)], None, &mut st);
        mir.store(1, 0, 0x5a);
        pg.complete(1);
        let out = std::env::temp_dir().join(format!("nxkit-mirror-{}", std::process::id()));
        let mut o = std::fs::File::create(&out).unwrap();
        let w = nxkit_abi::guest::with_slice(m.addr(1), 16, |b| {
            let r = o.write(b);
            (r, b[0])
        });
        let _ = std::fs::remove_file(&out);
        let (r, first) = w.unwrap();
        assert_eq!(r.unwrap(), 16);
        assert_eq!(first, 0x5a);
        nxkit_abi::guest::set_range(0, 0);
        pg.set_mirror(0);
    }

    #[test]
    fn eager_sync_copies_the_pages_back_when_the_frame_completes_and_writes_are_counted_by_phase() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, mir, pg) = mirrored(4);
        let mut st = Collect::default();
        // A member page that the CPU writes while the frame is flushed and not committed (phase 1).
        pg.note_committed(0);
        let _ = pg.flush_mirrored(1, &[(m.addr(2), 1)], &[(m.addr(1), 1)], None, &mut st);
        let s0 = pg.stats();
        m.store(2, 0, 4);
        let s1 = pg.stats();
        assert_eq!(s1.phase_writes[1] - s0.phase_writes[1], 1);
        assert_eq!(pg.take_between(1), (vec![m.addr(2)], 1), "the write is logged for the frame");
        // The late staging takes the page again.
        let late = pg.flush_late(&mut st);
        assert_eq!(late, vec![2]);
        assert_eq!(pg.state_of(m.addr(2)), "clean");
        pg.set_eager_sync(true);
        mir.store(1, 0, 9);
        pg.complete(1);
        assert_eq!(pg.state_of(m.addr(1)), "clean", "copied back without a CPU access");
        assert_eq!(m.load(1, 0), 9);
        pg.set_eager_sync(false);
        pg.note_committed(1);
        pg.set_mirror(0);
    }

    #[test]
    fn set_blocks_sets_the_blocks_a_byte_range_touches() {
        for (lo, hi) in [(0u64, 1u64), (0, 64), (1, 65), (63, 64), (100, 16384), (4000, 4096), (0, 16384), (4032, 4160), (8191, 8193)] {
            let mut got: Mask = [0; 4];
            set_blocks(&mut got, lo, hi);
            let mut want: Mask = [0; 4];
            for b in lo / BLOCK..=(hi - 1) / BLOCK {
                want[(b / 64) as usize] |= 1 << (b % 64);
            }
            assert_eq!(got, want, "{lo}..{hi}");
        }
    }

    #[test]
    fn runs_merge_and_the_lookup_finds_them() {
        assert_eq!(runs_of(&[1, 2, 3, 7, 8, 12]), vec![(1, 4), (7, 9), (12, 13)]);
        let r = vec![(1, 4), (7, 9)];
        assert!(in_runs(&r, 1) && in_runs(&r, 3) && in_runs(&r, 8));
        assert!(!in_runs(&r, 0) && !in_runs(&r, 4) && !in_runs(&r, 9));
    }

    /// An anonymous mapping (the alias needs real VM regions, not heap blocks).
    struct Map {
        base: *mut u8,
        len: usize,
    }

    impl Map {
        fn new(pages: u64) -> Self {
            let len = (pages * P) as usize;
            // SAFETY: an anonymous private mapping; the kernel picks the address.
            let base = unsafe { mmap(std::ptr::null_mut(), len, PROT_RW, MAP_PRIVATE | MAP_ANON, -1, 0) };
            assert!(base as isize != -1);
            Self { base: base.cast(), len }
        }

        fn addr(&self, page: u64) -> u64 {
            self.base as u64 + page * P
        }

        fn store(&self, page: u64, off: u64, v: u8) {
            // SAFETY: inside the mapping.
            unsafe { self.base.add((page * P + off) as usize).write_volatile(v) }
        }

        fn load(&self, page: u64, off: u64) -> u8 {
            // SAFETY: inside the mapping.
            unsafe { self.base.add((page * P + off) as usize).read_volatile() }
        }
    }

    impl Drop for Map {
        fn drop(&mut self) {
            // SAFETY: the mapping came from `mmap`; make it writable first so nothing is left protected.
            unsafe {
                mprotect(self.base.cast(), self.len, PROT_RW);
                munmap(self.base.cast(), self.len);
            }
        }
    }

    /// A tracker over a mapped guest and a mapped mirror, with the alias.
    fn emulating(pages: u64) -> (Map, Map, &'static Pages) {
        let m = Map::new(pages);
        let mir = Map::new(pages);
        let pg = Pages::global();
        pg.attach(m.addr(0), pages * P);
        pg.set_mirror(mir.addr(0));
        pg.make_alias().expect("alias");
        pg.set_emulation(true);
        (m, mir, pg)
    }

    fn pat(i: u64) -> u8 {
        (i * 7 + 3) as u8
    }

    #[test]
    fn the_alias_is_the_same_memory_with_its_own_protection() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, _mir, pg) = emulating(4);
        m.store(1, 9, 77);
        let alias = pg.alias_page(1).expect("alias");
        assert_eq!(alias[9], 77, "a store through the guest mapping shows in the alias");
        // The guest page loses all access; the alias still reads and writes it.
        protect_pages(1, 2, PROT_NONE, false);
        let alias_w = alias.as_ptr().cast_mut();
        // SAFETY: inside the alias page, which is writable.
        unsafe { alias_w.add(10).write_volatile(78) };
        protect_pages(1, 2, PROT_RW, false);
        assert_eq!(m.load(1, 10), 78, "a store through the alias shows in guest memory");
        pg.set_mirror(0);
    }

    #[test]
    fn written_ranges_become_blocks_rounded_outward() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, _mir, pg) = emulating(8);
        let ranges = [
            (m.addr(1) + 100, 8),               // block 1
            (m.addr(1) + 127, 2),               // blocks 1 and 2
            (m.addr(2) + 16, 3 * P),            // from block 0 of page 2 to block 0 of page 5, partly
            (m.addr(6), 1),                     // block 0 of page 6
        ];
        let runs = span_runs(&ranges);
        let masks = range_masks(&runs, &ranges);
        assert_eq!(runs, vec![(1, 6), (6, 7)].into_iter().fold(Vec::new(), |mut v: Vec<(u32, u32)>, r| {
            match v.last_mut() {
                Some(l) if l.1 == r.0 => l.1 = r.1,
                _ => v.push(r),
            }
            v
        }));
        let at = |pg: u32| masks[0][(pg - 1) as usize];
        assert_eq!(at(1), [0b110, 0, 0, 0]);
        assert_eq!(at(2), [u64::MAX; 4]);
        assert_eq!(at(3), [u64::MAX; 4]);
        assert_eq!(at(4), [u64::MAX; 4]);
        assert_eq!(at(5), [0b1, 0, 0, 0], "the range ends 16 bytes into page 5");
        assert_eq!(masks[0][5], [0b1, 0, 0, 0]);
        pg.set_mirror(0);
    }

    /// Loads and stores of several forms at `r` (a region of 256 bytes), from a thread of the test. Returns what the
    /// loads got.
    ///
    /// # Safety
    /// `r` must be valid for 256 bytes of reads and writes.
    #[allow(clippy::type_complexity)]
    unsafe fn exercise(r: *mut u8) -> (u64, u64, u64, i64, u64, u64, u64, u64, u64, u64) {
        use std::arch::asm;
        let (x0, x1, x2, sw, b, qlo, qhi, d, pre, post): (u64, u64, u64, i64, u64, u64, u64, u64, u64, u64);
        let (mut p1, mut p2) = (r, r);
        // SAFETY: per the caller, the accesses lie inside the region; the registers are declared.
        unsafe {
            asm!(
                "ldr {x0}, [{r}, #0]",
                "ldp {x1}, {x2}, [{r}, #8]",
                "ldrsw {sw}, [{r}, #24]",
                "ldrb {b:w}, [{r}, #28]",
                "ldr q0, [{r}, #32]",
                "fmov {qlo}, d0",
                "mov {qhi}, v0.d[1]",
                "ldr d1, [{r}, #48]",
                "fmov {d}, d1",
                "ldr {pre}, [{p1}, #56]!",
                "ldr {post}, [{p2}], #8",
                r = in(reg) r,
                p1 = inout(reg) p1,
                p2 = inout(reg) p2,
                x0 = out(reg) x0, x1 = out(reg) x1, x2 = out(reg) x2, sw = out(reg) sw, b = out(reg) b,
                qlo = out(reg) qlo, qhi = out(reg) qhi, d = out(reg) d, pre = out(reg) pre, post = out(reg) post,
                out("v0") _, out("v1") _,
                options(nostack),
            );
        }
        assert_eq!(p1 as usize, r as usize + 56, "the pre-index wrote the base back");
        assert_eq!(p2 as usize, r as usize + 8, "the post-index wrote the base back");
        // Stores into the second half of the region.
        let (s0, s1, s2) = (0x1122_3344_5566_7788u64, 0x99aa_bbcc_ddee_ff00u64, 0xa5u64);
        // SAFETY: as above.
        unsafe {
            asm!(
                "str {s0}, [{r}, #128]",
                "stp {s0}, {s1}, [{r}, #136]",
                "fmov d0, {s0}",
                "mov v0.d[1], {s1}",
                "str q0, [{r}, #160]",
                "strh {s2:w}, [{r}, #176]",
                "stur {s0:w}, [{r}, #181]",
                "stp q0, q0, [{r}, #192]",
                "str d0, [{r}, #224]",
                "strb {s2:w}, [{r}, #232]",
                r = in(reg) r,
                s0 = in(reg) s0, s1 = in(reg) s1, s2 = in(reg) s2,
                out("v0") _,
                options(nostack),
            );
        }
        (x0, x1, x2, sw, b, qlo, qhi, d, pre, post)
    }

    fn le(bytes: impl Fn(u64) -> u8, at: u64, n: u64) -> u64 {
        (0..n).fold(0u64, |a, k| a | u64::from(bytes(at + k)) << (8 * k))
    }

    #[test]
    fn accesses_outside_the_written_bytes_are_done_by_the_handler_on_several_threads_without_a_wait() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, mir, pg) = emulating(4);
        for i in 0..P {
            m.store(1, i, pat(i));
        }
        let mut st = Collect::default();
        // The frame may write the first 200 bytes of page 1: blocks 0 to 3.
        let f = pg.flush_mirrored(1, &[], &[(m.addr(1), 200)], None, &mut st);
        assert_eq!((f.new_members, f.written), (1, 1));
        assert_eq!(pg.state_of(m.addr(1)), "gpu(1)");
        let s0 = pg.stats();
        let base = m.addr(1) as usize;
        let handles: Vec<_> = (0..4usize)
            .map(|t| {
                std::thread::spawn(move || {
                    let off = 1024 + 1024 * t as u64;
                    let mut last = None;
                    for _ in 0..50 {
                        // SAFETY: the region lies inside page 1 of the test mapping, which outlives the joined thread.
                        last = Some(unsafe { exercise((base + off as usize) as *mut u8) });
                    }
                    (off, last)
                })
            })
            .collect();
        for h in handles {
            let (off, last) = h.join().unwrap();
            let (x0, x1, x2, sw, b, qlo, qhi, d, pre, post) = last.unwrap();
            let g = |i: u64| pat(off + i);
            assert_eq!(x0, le(g, 0, 8));
            assert_eq!((x1, x2), (le(g, 8, 8), le(g, 16, 8)));
            assert_eq!(sw, i64::from(le(g, 24, 4) as u32 as i32));
            assert_eq!(b, u64::from(g(28)));
            assert_eq!((qlo, qhi), (le(g, 32, 8), le(g, 40, 8)));
            assert_eq!(d, le(g, 48, 8));
            assert_eq!(pre, le(g, 56, 8));
            assert_eq!(post, le(g, 0, 8), "the post-index load reads before the base moves");
            // The stores landed in guest memory (through the alias).
            let alias = pg.alias_page(1).unwrap();
            let at = |i: u64| alias[(off + i) as usize];
            let word = |i: u64, n: u64| le(at, i, n);
            assert_eq!(word(128, 8), 0x1122_3344_5566_7788);
            assert_eq!(word(136, 8), 0x1122_3344_5566_7788);
            assert_eq!(word(144, 8), 0x99aa_bbcc_ddee_ff00);
            assert_eq!(word(160, 8), 0x1122_3344_5566_7788);
            assert_eq!(word(168, 8), 0x99aa_bbcc_ddee_ff00);
            assert_eq!(word(176, 2), 0x00a5);
            assert_eq!(word(181, 4), 0x5566_7788);
            assert_eq!(word(192, 8), 0x1122_3344_5566_7788);
            assert_eq!(word(216, 8), 0x99aa_bbcc_ddee_ff00);
            assert_eq!(word(224, 8), 0x1122_3344_5566_7788);
            assert_eq!(at(232), 0xa5);
        }
        let s1 = pg.stats();
        let total = |s: &PageStats| s.emulated.iter().sum::<u64>();
        assert_eq!(total(&s1) - total(&s0), 4 * 50 * 16, "every access was done by the handler");
        assert_eq!(s1.fallbacks, s0.fallbacks, "none fell back");
        assert_eq!(s1.writer_waits, s0.writer_waits, "no wait for the writer happened");
        assert_eq!(s1.waits, s0.waits);
        assert_eq!(s1.faults, s0.faults, "the faults were not served by the ordinary path");
        assert_eq!(pg.state_of(m.addr(1)), "gpu(1)", "the page stayed closed");
        // The GPU wrote the first 200 bytes of the mirror meanwhile.
        for i in 0..200 {
            mir.store(1, i, 0xee);
        }
        // The next flush stages the page with the mask of the stale blocks, and the stores are in the bytes.
        let mut st2 = Collect::default();
        let f2 = pg.flush_mirrored(2, &[], &[], None, &mut st2);
        assert_eq!((f2.staged, f2.emu_staged), (1, 1));
        assert_eq!(st2.skips, vec![(1, [0b1111, 0, 0, 0])]);
        assert_eq!(st2.pages[0].1[(1024 + 128) as usize], 0x88, "the staged bytes hold the CPU's stores");
        assert_eq!(pg.stats().emu_staged - s1.emu_staged, 1);
        // Nothing new was stored: the page is not staged again.
        let mut st3 = Collect::default();
        let f3 = pg.flush_mirrored(3, &[], &[], None, &mut st3);
        assert_eq!(f3.staged, 0);
        // The frame completes; a read copies the written blocks only: the stores stay, the GPU's bytes arrive.
        pg.complete(3);
        let sync0 = pg.stats().synced;
        assert_eq!(m.load(1, 5), 0xee, "the GPU's byte is copied back");
        assert_eq!(m.load(1, 199), 0xee);
        assert_eq!(m.load(1, 200), pat(200), "a byte outside the blocks is not touched");
        for t in 0..4u64 {
            let off = 1024 + 1024 * t;
            assert_eq!(m.load(1, off + 128), 0x88, "the store survives the copy back");
            assert_eq!(m.load(1, off + 232), 0xa5);
            assert_eq!(m.load(1, off + 8), pat(off + 8), "so do the bytes that were only read");
        }
        assert_eq!(pg.stats().synced - sync0, 1);
        assert_eq!(pg.state_of(m.addr(1)), "clean");
        pg.set_mirror(0);
    }

    #[test]
    fn an_access_that_touches_the_written_bytes_waits_and_is_counted() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, mir, pg) = emulating(4);
        let mut st = Collect::default();
        let _ = pg.flush_mirrored(7, &[], &[(m.addr(1) + 64, 64)], None, &mut st);
        mir.store(1, 70, 1);
        let s0 = pg.stats();
        let base = m.addr(1) as usize;
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let d2 = done.clone();
        // An 8-byte load whose last byte is in the written block (bytes 56 to 63 are block 0, 64 to 127 block 1).
        let reader = std::thread::spawn(move || {
            let v: u64;
            // SAFETY: inside page 1 of the test mapping, which outlives the joined thread.
            unsafe {
                std::arch::asm!("ldr {v}, [{p}, #60]", p = in(reg) base, v = out(reg) v, options(nostack));
            }
            d2.store(true, Ordering::Release);
            v
        });
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert!(!done.load(Ordering::Acquire), "an access that touches written bytes waits for the frame");
        mir.store(1, 66, 0x42);
        pg.complete(7);
        let v = reader.join().unwrap();
        assert_eq!((v >> 48) & 0xff, 0x42, "the load saw the GPU's byte");
        let s1 = pg.stats();
        assert_eq!(s1.fallbacks[Reason::Overlap as usize] - s0.fallbacks[Reason::Overlap as usize], 1);
        assert_eq!(s1.writer_waits - s0.writer_waits, 1);
        assert_eq!(s1.emulated, s0.emulated);
        pg.set_mirror(0);
    }

    #[test]
    fn what_cannot_be_emulated_waits_and_is_counted_by_class() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, mir, pg) = emulating(4);
        let mut st = Collect::default();
        let _ = pg.flush_mirrored(9, &[], &[(m.addr(1), 64)], None, &mut st);
        mir.store(1, 0, 1);
        let s0 = pg.stats();
        let base = m.addr(1) as usize + 1024;
        let reader = std::thread::spawn(move || {
            // SAFETY: inside page 1 of the test mapping, which outlives the joined thread; LD2 is not emulated.
            unsafe {
                std::arch::asm!("ld2 {{v0.16b, v1.16b}}, [{p}]", p = in(reg) base, out("v0") _, out("v1") _, options(nostack));
            }
            0u64
        });
        std::thread::sleep(std::time::Duration::from_millis(40));
        pg.complete(9);
        let _ = reader.join().unwrap();
        let s1 = pg.stats();
        assert_eq!(s1.fallbacks[Reason::Structure as usize] - s0.fallbacks[Reason::Structure as usize], 1);
        assert_eq!(s1.writer_waits - s0.writer_waits, 1, "it waited for the writer");
        assert_eq!(s1.emulated, s0.emulated);
        pg.set_mirror(0);
    }

    #[test]
    fn with_the_emulation_off_every_access_waits() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, _mir, pg) = emulating(4);
        let mut st = Collect::default();
        let _ = pg.flush_mirrored(11, &[], &[(m.addr(1), 64)], None, &mut st);
        pg.set_emulation(false);
        assert!(!pg.emulating());
        let base = m.addr(1) as usize + 1024;
        let s0 = pg.stats();
        let reader = std::thread::spawn(move || {
            // SAFETY: inside page 1 of the test mapping, which outlives the joined thread.
            unsafe { (base as *const u8).read_volatile() }
        });
        std::thread::sleep(std::time::Duration::from_millis(40));
        pg.complete(11);
        reader.join().unwrap();
        let s1 = pg.stats();
        assert_eq!(s1.writer_waits - s0.writer_waits, 1);
        assert_eq!(s1.emulated, s0.emulated);
        pg.set_emulation(true);
        pg.set_mirror(0);
    }

    #[test]
    fn a_store_across_the_page_border_and_a_lane_store_are_done_by_the_handler_without_a_wait() {
        use std::arch::asm;
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, _mir, pg) = emulating(4);
        let mut st = Collect::default();
        // Page 1 is in flight (bytes 0 to 255 are the frame's), page 2 is clean and tracked.
        let _ = pg.flush_mirrored(1, &[(m.addr(2), P)], &[(m.addr(1), 256)], None, &mut st);
        let s0 = pg.stats();
        let border = (m.addr(1) + P) as usize;
        // SAFETY: the pair lies on the border of pages 1 and 2 of the test mapping; the handler does the store.
        unsafe {
            asm!("stp {a}, {b}, [{p}]", p = in(reg) border - 8, a = in(reg) 0x1122_3344_5566_7788u64,
                b = in(reg) 0x99aa_bbcc_ddee_ff00u64, options(nostack));
            // `st1 {v0.s}[1], [x]` in the page in flight, outside the frame's bytes.
            asm!("movi v0.4s, #7", "st1 {{v0.s}}[1], [{p}]", p = in(reg) m.addr(1) as usize + 1024, out("v0") _,
                options(nostack));
        }
        let s1 = pg.stats();
        assert_eq!(s1.writer_waits, s0.writer_waits, "no wait");
        assert_eq!(s1.fallbacks[Reason::Mismatch as usize], s0.fallbacks[Reason::Mismatch as usize]);
        assert_eq!(s1.emulated[Form::St1 as usize] - s0.emulated[Form::St1 as usize], 1);
        let a1 = pg.alias_page(1).unwrap();
        let a2 = pg.alias_page(2).unwrap();
        assert_eq!(&a1[(P - 8) as usize..], &0x1122_3344_5566_7788u64.to_le_bytes());
        assert_eq!(&a2[..8], &0x99aa_bbcc_ddee_ff00u64.to_le_bytes());
        assert_eq!(&a1[1024..1028], &7u32.to_le_bytes());
        // The flush stages the clean page that the handler stored to.
        let mut st2 = Collect::default();
        let f = pg.flush_mirrored(2, &[], &[], None, &mut st2);
        assert!(f.emu_staged >= 2, "pages 1 and 2 were stored to: {f:?}");
        pg.complete(1);
        pg.complete(2);
    }

    #[test]
    fn atomics_exclusives_and_ld1_outside_the_written_bytes_are_done_by_the_handler_and_sum_correctly() {
        use std::arch::asm;
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, _mir, pg) = emulating(4);
        let mut st = Collect::default();
        // The frame may write bytes 0 to 255; everything below is outside.
        let _ = pg.flush_mirrored(1, &[], &[(m.addr(1), 256)], None, &mut st);
        let s0 = pg.stats();
        let base = m.addr(1) as usize;
        const N: u64 = 300;
        // The sources of the LD1 copies, written through the alias (the page has no access).
        let a0 = pg.alias_page(1).unwrap().as_ptr().cast_mut();
        for k in 0..4 * 64 {
            // SAFETY: inside the alias page, which is writable.
            unsafe { a0.add(1024 + 128 + k).write(k as u8 ^ 0x3c) };
        }
        let handles: Vec<_> = (0..4u64)
            .map(|t| {
                std::thread::spawn(move || {
                    let p = base + 1024;
                    for i in 0..N {
                        // SAFETY: the counters lie inside page 1 of the test mapping, which outlives the joined threads;
                        // the offsets are aligned for their accesses.
                        unsafe {
                            // LDADD (add one), through the acquire-release form.
                            asm!(".arch_extension lse", "ldaddal {one}, {old}, [{p}]",
                                p = in(reg) p, one = in(reg) 1u64, old = out(reg) _, options(nostack));
                            // STADD of a word (no result).
                            asm!(".arch_extension lse", "stadd {one:w}, [{p}]",
                                p = in(reg) p + 8, one = in(reg) 1u32, options(nostack));
                            // LDXR/STXR increment loop.
                            asm!("2:", "ldaxr {t}, [{p}]", "add {t}, {t}, #1", "stlxr {s:w}, {t}, [{p}]", "cbnz {s:w}, 2b",
                                p = in(reg) p + 16, t = out(reg) _, s = out(reg) _, options(nostack));
                            // CAS increment loop.
                            asm!(".arch_extension lse", "ldr {t}, [{p}]", "2:", "add {n}, {t}, #1", "mov {e}, {t}",
                                "casal {e}, {n}, [{p}]", "cmp {e}, {t}", "mov {t}, {e}", "b.ne 2b",
                                p = in(reg) p + 24, t = out(reg) _, n = out(reg) _, e = out(reg) _, options(nostack));
                            // LDUMAX of a value that grows with the thread and the round.
                            asm!(".arch_extension lse", "ldumax {v}, {old}, [{p}]",
                                p = in(reg) p + 32, v = in(reg) t * 1000 + i, old = out(reg) _, options(nostack));
                            // LDSMIN of a negative value, LDSET of a bit per thread, LDEOR twice (back to zero).
                            asm!(".arch_extension lse", "ldsmin {v}, {old}, [{p}]",
                                p = in(reg) p + 40, v = in(reg) (-(t as i64) - 1) as u64, old = out(reg) _, options(nostack));
                            asm!(".arch_extension lse", "ldset {v}, {old}, [{p}]",
                                p = in(reg) p + 48, v = in(reg) 1u64 << t, old = out(reg) _, options(nostack));
                            asm!(".arch_extension lse", "ldeor {v}, {old}, [{p}]", "ldeor {v}, {old}, [{p}]",
                                p = in(reg) p + 56, v = in(reg) 0xffu64 << (8 * t), old = out(reg) _, options(nostack));
                            // SWP of the thread number.
                            asm!(".arch_extension lse", "swp {v}, {old}, [{p}]",
                                p = in(reg) p + 64, v = in(reg) t, old = out(reg) _, options(nostack));
                            // CASP increment loop on a pair of x registers (16-byte aligned).
                            asm!(".arch_extension lse", "ldp x8, x9, [{p}]", "2:", "add x12, x8, #1", "add x13, x9, #2",
                                "mov x10, x8", "mov x11, x9", "caspal x10, x11, x12, x13, [{p}]",
                                "cmp x10, x8", "ccmp x11, x9, #0, eq", "mov x8, x10", "mov x9, x11", "b.ne 2b",
                                p = in(reg) p + 80, out("x8") _, out("x9") _, out("x10") _, out("x11") _,
                                out("x12") _, out("x13") _, options(nostack));
                            // LDXP/STXP increment loop.
                            asm!("2:", "ldaxp x8, x9, [{p}]", "add x8, x8, #1", "add x9, x9, #3",
                                "stlxp {s:w}, x8, x9, [{p}]", "cbnz {s:w}, 2b",
                                p = in(reg) p + 96, s = out(reg) _, out("x8") _, out("x9") _, options(nostack));
                            // LD1/ST1 of two registers, with a post index on the store.
                            let src = p + 128 + 64 * t as usize;
                            asm!("ld1 {{v0.16b, v1.16b}}, [{s}]", "st1 {{v0.16b, v1.16b}}, [{d}], #32",
                                s = in(reg) src, d = inout(reg) src + 32 => _, out("v0") _, out("v1") _, options(nostack));
                        }
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let alias = pg.alias_page(1).unwrap();
        let w = |off: usize| u64::from_le_bytes(alias[1024 + off..1024 + off + 8].try_into().unwrap());
        assert_eq!(w(0), 4 * N, "ldadd");
        assert_eq!(w(8) & 0xffff_ffff, 4 * N, "stadd");
        for t in 0..4usize {
            for k in 0..32 {
                let src = 1024 + 128 + 64 * t + k;
                assert_eq!(alias[src + 32], alias[src], "ld1/st1 copy, thread {t} byte {k}");
            }
        }
        assert_eq!(w(16), 4 * N, "ldxr/stxr");
        assert_eq!(w(24), 4 * N, "cas");
        assert_eq!(w(32), 3 * 1000 + N - 1, "ldumax");
        assert_eq!(w(40), (-4i64) as u64, "ldsmin");
        assert_eq!(w(48), 0b1111, "ldset");
        assert_eq!(w(56), 0, "ldeor twice");
        assert!(w(64) < 4, "swp");
        assert_eq!((w(80), w(88)), (4 * N, 8 * N), "casp");
        assert_eq!((w(96), w(104)), (4 * N, 12 * N), "ldxp/stxp");
        let s1 = pg.stats();
        let n = |s: &PageStats, f: Form| s.emulated[f as usize];
        assert_eq!(n(&s1, Form::Rmw) - n(&s0, Form::Rmw), 4 * N * 8);
        assert!(n(&s1, Form::Cas) - n(&s0, Form::Cas) >= 4 * N * 2);
        assert!(n(&s1, Form::Ldxr) - n(&s0, Form::Ldxr) >= 4 * N * 2);
        assert_eq!(n(&s1, Form::Ld1) - n(&s0, Form::Ld1), 4 * N);
        assert_eq!(n(&s1, Form::St1) - n(&s0, Form::St1), 4 * N);
        assert_eq!(s1.fallbacks, s0.fallbacks, "none fell back");
        assert_eq!(s1.writer_waits, s0.writer_waits);
        assert_eq!(pg.state_of(m.addr(1)), "gpu(1)");
        pg.set_mirror(0);
    }

    /// A page in step 1 of the late-results tests: frame 1 wrote the bytes `[64, 192)` of page 1, the GPU's result of it is
    /// `a1` at byte 70 and `a2` at byte 130, the frame completed, and a read copied the page (so the page is a CPU-read GPU
    /// result and guest memory holds frame 1).
    fn late_page(a1: u8, a2: u8) -> (Map, Map, &'static Pages) {
        let (m, mir, pg) = emulating(4);
        pg.set_late(true);
        let mut st = Collect::default();
        let _ = pg.flush_mirrored(1, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
        mir.store(1, 70, a1);
        mir.store(1, 130, a2);
        pg.complete(1);
        assert_eq!(m.load(1, 70), a1, "the first read of a result waits for nothing (the frame is done) and copies the page");
        assert_eq!(m.load(1, 130), a2);
        (m, mir, pg)
    }

    fn done(pg: &Pages) {
        pg.set_late(false);
        pg.set_adaptive(false, false);
        pg.set_mirror(0);
    }

    #[test]
    fn a_reader_during_a_frame_gets_the_last_completed_frames_bytes_at_once_and_the_frames_after_it_completes() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, mir, pg) = late_page(0xA1, 0xA2);
        let mut st = Collect::default();
        let _ = pg.flush_mirrored(2, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
        // The frame in flight writes the mirror.
        mir.store(1, 70, 0xB1);
        mir.store(1, 130, 0xB2);
        let s0 = pg.stats();
        let t0 = now_ns();
        assert_eq!(m.load(1, 70), 0xA1, "frame 1's byte, not frame 2's");
        assert_eq!(m.load(1, 130), 0xA2);
        assert!(now_ns() - t0 < 500_000_000, "no wait");
        let s1 = pg.stats();
        assert_eq!(s1.late_reads - s0.late_reads, 2);
        assert_eq!(s1.late_age[0] - s0.late_age[0], 2, "the bytes are one frame old");
        assert_eq!(s1.writer_waits, s0.writer_waits);
        assert_eq!(s1.fallbacks[Reason::Overlap as usize], s0.fallbacks[Reason::Overlap as usize]);
        assert_eq!(pg.state_of(m.addr(1)), "gpu(2)", "the page keeps its tag and has no access");
        pg.complete(2);
        assert_eq!(m.load(1, 70), 0xB1, "after the completion the reader gets frame 2's bytes");
        assert_eq!(m.load(1, 130), 0xB2);
        done(pg);
    }

    #[test]
    fn the_completion_of_a_frame_copies_its_bytes_for_the_readers_of_the_frame_that_runs_ahead() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, mir, pg) = late_page(0xA1, 0xA2);
        let mut st = Collect::default();
        let _ = pg.flush_mirrored(2, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
        mir.store(1, 70, 0xB1);
        // The CPU runs ahead: frame 3 is flushed while frame 2 runs, and tags the page.
        let _ = pg.flush_mirrored(3, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
        assert_eq!(m.load(1, 70), 0xA1, "frame 3 and 2 run: the bytes are frame 1's");
        let s0 = pg.stats();
        pg.complete(2);
        let s1 = pg.stats();
        assert_eq!(s1.late_copied - s0.late_copied, 1, "the completion copied the page");
        assert_eq!(m.load(1, 70), 0xB1, "frame 3 runs: the bytes are frame 2's now");
        assert_eq!(pg.state_of(m.addr(1)), "gpu(3)");
        assert_eq!(pg.stats().writer_waits, s0.writer_waits, "nobody waited");
        mir.store(1, 70, 0xC1);
        pg.complete(3);
        assert_eq!(m.load(1, 70), 0xC1);
        done(pg);
    }

    #[test]
    fn the_completion_of_the_newest_frame_copies_its_bytes_before_the_next_frame_is_flushed() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, mir, pg) = late_page(0xA1, 0xA2);
        let mut st = Collect::default();
        let _ = pg.flush_mirrored(2, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
        mir.store(1, 70, 0xB1);
        // The CPU runs one frame ahead at most: frame 2 completes before frame 3 is flushed, and nothing tags the page.
        let s0 = pg.stats();
        pg.complete(2);
        let s1 = pg.stats();
        assert_eq!(s1.late_copied - s0.late_copied, 1, "the completion copied the page of the newest tag");
        assert_eq!(pg.state_of(m.addr(1)), "gpu(2)");
        let _ = pg.flush_mirrored(3, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
        assert_eq!(m.load(1, 70), 0xB1, "frame 3 runs: the reader gets frame 2's bytes without a wait");
        assert_eq!(pg.stats().writer_waits, s0.writer_waits, "nobody waited");
        pg.complete(3);
        done(pg);
    }

    #[test]
    fn a_snapshot_dropped_at_a_completion_is_counted_with_its_reason_and_the_next_access_names_it() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, mir, pg) = late_page(0xA1, 0xA2);
        let mut st = Collect::default();
        // Frame 600 is in another epoch than the read of the page (epoch 0): nobody read the page lately.
        let _ = pg.flush_mirrored(2, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
        let _ = pg.flush_mirrored(600, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
        let s0 = pg.stats();
        pg.complete(2);
        let s1 = pg.stats();
        assert_eq!(s1.snap_resets[SnapWhy::Idle as usize] - s0.snap_resets[SnapWhy::Idle as usize], 1);
        mir.store(1, 70, 0xB1);
        let base = m.addr(1) as usize;
        let reader = std::thread::spawn(move || {
            // SAFETY: inside page 1 of the test mapping, which outlives the joined thread.
            unsafe { ((base + 70) as *const u8).read_volatile() }
        });
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert!(!reader.is_finished(), "no snapshot: the reader waits for the writer");
        pg.complete(600);
        assert_eq!(reader.join().unwrap(), 0xB1);
        let s2 = pg.stats();
        assert_eq!(s2.snap_misses[SnapWhy::Idle as usize] - s1.snap_misses[SnapWhy::Idle as usize], 1);
        done(pg);
    }

    #[test]
    fn a_page_with_a_snapshot_stays_readable_while_a_frame_writes_it_and_only_stores_fault() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, mir, pg) = late_page(0xA1, 0xA2);
        pg.set_read_open(true);
        let mut st = Collect::default();
        let s0 = pg.stats();
        let _ = pg.flush_mirrored(2, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
        mir.store(1, 70, 0xB1);
        let s1 = pg.stats();
        assert_eq!(s1.read_open - s0.read_open, 1, "the flush left the page readable");
        // Reads of the stale blocks and of the other bytes take no trap: nothing is counted by the handler.
        let (emu0, handler0) = (s1.emulated.iter().sum::<u64>(), s1.late_reads);
        assert_eq!(m.load(1, 70), 0xA1, "frame 1's bytes while frame 2 runs");
        assert_eq!(m.load(1, 300), 0);
        let s2 = pg.stats();
        assert_eq!(s2.emulated.iter().sum::<u64>(), emu0, "no emulated access: no fault");
        assert_eq!(s2.late_reads, handler0);
        // A store faults and is done by the handler: the blocks become the CPU's.
        m.store(1, 70, 0xC7);
        let s3 = pg.stats();
        assert_eq!(s3.late_writes - s2.late_writes, 1);
        assert_eq!(s3.writer_waits, s0.writer_waits, "nobody waited");
        pg.complete(2);
        done(pg);
        pg.set_read_open(false);
    }

    #[test]
    fn a_readable_page_nobody_reads_lately_loses_its_access_at_the_completion() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, _mir, pg) = late_page(0xA1, 0xA2);
        pg.set_read_open(true);
        let mut st = Collect::default();
        let _ = pg.flush_mirrored(2, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
        // Frame 600 is in another epoch: when frame 2 completes, the page is not kept current any more.
        let _ = pg.flush_mirrored(600, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
        let s0 = pg.stats();
        pg.complete(2);
        let s1 = pg.stats();
        assert_eq!(s1.read_revoked - s0.read_revoked, 1);
        let base = m.addr(1) as usize;
        let reader = std::thread::spawn(move || {
            // SAFETY: inside page 1 of the test mapping, which outlives the joined thread.
            unsafe { ((base + 70) as *const u8).read_volatile() }
        });
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert!(!reader.is_finished(), "the page lost its read access: the reader waits");
        pg.complete(600);
        reader.join().unwrap();
        done(pg);
        pg.set_read_open(false);
    }

    #[test]
    fn a_system_call_read_outside_the_written_bytes_of_a_page_with_a_snapshot_does_not_wait() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, _mir, pg) = late_page(0xA1, 0xA2);
        let mut st = Collect::default();
        let _ = pg.flush_mirrored(2, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
        let s0 = pg.stats();
        hook(m.addr(1) + 300, 8, nxkit_abi::guest::ACCESS_READ);
        let s1 = pg.stats();
        assert_eq!(s1.settle_late - s0.settle_late, 1);
        assert_eq!(s1.settle_waits, s0.settle_waits);
        assert_eq!(m.load(1, 300), 0, "the page is readable for the system call");
        pg.complete(2);
        done(pg);
    }

    #[test]
    fn a_page_that_nobody_waited_for_still_waits() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, mir, pg) = emulating(4);
        pg.set_late(true);
        let mut st = Collect::default();
        let _ = pg.flush_mirrored(1, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
        mir.store(1, 70, 5);
        let s0 = pg.stats();
        let base = m.addr(1) as usize;
        let reader = std::thread::spawn(move || {
            // SAFETY: inside page 1 of the test mapping, which outlives the joined thread.
            unsafe { ((base + 70) as *const u8).read_volatile() }
        });
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert!(!reader.is_finished(), "the guest bytes are not known to be a completed frame's: it waits");
        pg.complete(1);
        assert_eq!(reader.join().unwrap(), 5);
        let s1 = pg.stats();
        assert_eq!(s1.late_reads, s0.late_reads);
        assert_eq!(s1.writer_waits - s0.writer_waits, 1);
        done(pg);
    }

    /// Waits until the copy-back of the completed frame has opened the page.
    fn wait_clean(pg: &Pages, m: &Map) {
        for _ in 0..400 {
            if pg.state_of(m.addr(1)) == "clean" {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("the copy-back did not run: {}", pg.state_of(m.addr(1)));
    }

    #[test]
    fn a_cpu_write_during_a_frame_survives_the_copy_back_of_the_frame_and_is_staged_at_the_next_flush() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, mir, pg) = late_page(0xA1, 0xA2);
        pg.set_adaptive(true, false);
        let mut st = Collect::default();
        let _ = pg.flush_mirrored(2, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
        mir.store(1, 70, 0xB1);
        mir.store(1, 130, 0xB2);
        let s0 = pg.stats();
        m.store(1, 70, 0x55);
        let s1 = pg.stats();
        assert_eq!(s1.late_writes - s0.late_writes, 1);
        assert_eq!(s1.late_owned - s0.late_owned, 1, "one 64-byte block");
        assert_eq!(s1.writer_waits, s0.writer_waits);
        assert_eq!(m.load(1, 70), 0x55, "the CPU reads what it wrote");
        pg.complete(2);
        wait_clean(pg, &m);
        assert_eq!(m.load(1, 70), 0x55, "the copy-back of frame 2 left the block the CPU wrote");
        assert_eq!(m.load(1, 130), 0xB2, "and copied the other block");
        assert!(pg.stats().late_kept > s1.late_kept);
        // The next flush stages the page with the CPU's byte.
        let mut st3 = Collect::default();
        let f = pg.flush_mirrored(3, &[], &[(m.addr(1) + 64, 128)], None, &mut st3);
        assert_eq!(f.staged, 1);
        let i = st3.pages.iter().position(|p| p.0 == 1).expect("the page is staged");
        assert_eq!(st3.pages[i].1[70], 0x55);
        assert_eq!(st3.skips[i].1, [0; 4], "the page is current in guest memory: nothing is skipped");
        pg.complete(3);
        pg.set_adaptive(false, false);
        done(pg);
    }

    #[test]
    fn a_write_staged_while_its_frame_runs_skips_the_blocks_only_the_frame_wrote_and_survives_the_frames_completion() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, mir, pg) = late_page(0xA1, 0xA2);
        let mut st = Collect::default();
        let _ = pg.flush_mirrored(2, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
        mir.store(1, 70, 0xB1);
        mir.store(1, 130, 0xB2);
        m.store(1, 70, 0x55);
        // The CPU runs ahead: the flush of frame 3 stages the page while frame 2 runs.
        let mut st3 = Collect::default();
        let f = pg.flush_mirrored(3, &[], &[(m.addr(1) + 64, 128)], None, &mut st3);
        assert_eq!(f.emu_staged, 1);
        let i = st3.pages.iter().position(|p| p.0 == 1).expect("the page is staged");
        assert_eq!(st3.pages[i].1[70], 0x55);
        // Block 1 (bytes 64 to 127) is the CPU's and goes along; block 2 (128 to 191) is the frame's and is skipped.
        assert_eq!(st3.skips[i].1[0], 0b100, "only the block of the frame is skipped");
        // The frame completes while the page is tagged by frame 3: its copy-back keeps the staged block.
        pg.complete(2);
        assert_eq!(m.load(1, 70), 0x55, "the copy-back of frame 2 does not take back what the CPU wrote before frame 3");
        assert_eq!(m.load(1, 130), 0xB2, "the other block is frame 2's");
        // The staged block is not staged again at the next flush: a frame may have written it since.
        let mut st4 = Collect::default();
        let _ = pg.flush_mirrored(4, &[], &[(m.addr(1) + 64, 128)], None, &mut st4);
        assert!(st4.pages.iter().all(|p| p.0 != 1), "nothing new to stage");
        pg.complete(3);
        pg.complete(4);
        done(pg);
    }

    #[test]
    fn an_atomic_add_during_a_frame_lands_on_guest_memory_and_is_staged() {
        use std::arch::asm;
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, mir, pg) = emulating(4);
        pg.set_late(true);
        let mut st = Collect::default();
        // Frame 1's result: a counter of 10 at byte 64.
        let _ = pg.flush_mirrored(1, &[], &[(m.addr(1) + 64, 64)], None, &mut st);
        for (k, b) in 10u32.to_le_bytes().iter().enumerate() {
            mir.store(1, 64 + k as u64, *b);
        }
        pg.complete(1);
        assert_eq!(m.load(1, 64), 10);
        let _ = pg.flush_mirrored(2, &[], &[(m.addr(1) + 64, 64)], None, &mut st);
        for (k, b) in 99u32.to_le_bytes().iter().enumerate() {
            mir.store(1, 64 + k as u64, *b);
        }
        let s0 = pg.stats();
        let p = m.addr(1) as usize + 64;
        let old: u32;
        // SAFETY: inside page 1 of the test mapping, aligned for a word.
        unsafe {
            asm!(".arch_extension lse", "ldaddal {one:w}, {old:w}, [{p}]",
                p = in(reg) p, one = in(reg) 5u32, old = out(reg) old, options(nostack));
        }
        assert_eq!(old, 10, "the atomic operates on the last completed frame's value");
        let s1 = pg.stats();
        assert_eq!(s1.late_writes - s0.late_writes, 1);
        assert_eq!(s1.writer_waits, s0.writer_waits);
        let mut st3 = Collect::default();
        let _ = pg.flush_mirrored(3, &[], &[(m.addr(1) + 64, 64)], None, &mut st3);
        let i = st3.pages.iter().position(|p| p.0 == 1).expect("the page is staged");
        assert_eq!(u32::from_le_bytes(st3.pages[i].1[64..68].try_into().unwrap()), 15, "the sum is staged");
        assert_eq!(st3.skips[i].1[0] & 0b10, 0, "the block is not skipped");
        pg.complete(2);
        assert_eq!(u32::from_le_bytes([m.load(1, 64), m.load(1, 65), m.load(1, 66), m.load(1, 67)]), 15, "the CPU wins over frame 2's 99");
        pg.complete(3);
        done(pg);
    }

    #[test]
    fn with_late_results_off_an_access_to_written_bytes_waits() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, mir, pg) = late_page(0xA1, 0xA2);
        pg.set_late(false);
        let mut st = Collect::default();
        let _ = pg.flush_mirrored(2, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
        mir.store(1, 70, 0xB1);
        let s0 = pg.stats();
        let base = m.addr(1) as usize;
        let reader = std::thread::spawn(move || {
            // SAFETY: inside page 1 of the test mapping, which outlives the joined thread.
            unsafe { ((base + 70) as *const u8).read_volatile() }
        });
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert!(!reader.is_finished());
        pg.complete(2);
        assert_eq!(reader.join().unwrap(), 0xB1);
        assert_eq!(pg.stats().late_reads, s0.late_reads);
        done(pg);
    }
}
