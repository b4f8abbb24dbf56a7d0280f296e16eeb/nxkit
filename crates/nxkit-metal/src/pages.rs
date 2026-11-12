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
mod access;
mod api;
mod eager;
mod emulate;
mod fault;
mod flush;
mod flush_mirrored;
mod info;
mod late;
mod mirror;
mod runs;
mod shared;
mod signal;
mod stats;
mod sys;
#[cfg(test)]
mod tests;
mod wait;

pub use a64::{Form, Reason};
pub use fault::{InternalThread, mark_internal_thread};
pub use signal::{name_pc, set_pc_namer};
pub use stats::{FlushStats, PageStats};

use std::collections::{BTreeMap, VecDeque};
use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, Once, PoisonError};

use eager::{FrameMasks, FrameRuns, eager_quiesce, eager_submit};
use emulate::try_emulate;
use fault::{
    count_fallback, count_pc, count_pc_in, fault_regs, is_internal, log_dirty, page_span,
    protect_pages, thread_regs,
};
use info::stage_skip;
use late::{late_active, late_copy_back, late_current, late_host_read, stage_carry};
use mirror::{copy_blocks, copy_stale, has, serve_mirrored};
use runs::{close_opened, in_runs, range_masks, release_runs, runs_of, span_runs};
use shared::SHARED;
use signal::{build_alias, install_handler};
use sys::{
    DlInfo, Handler, MAP_ANON, MAP_NORESERVE, MAP_PRIVATE, SigAction, SigInfo, VM_FLAGS_FIXED,
    VM_FLAGS_OVERWRITE, VM_INHERIT_NONE, clock_gettime_nsec_np, dladdr, getpagesize,
    mach_task_self_, mach_vm_protect, mach_vm_remap, mmap, mprotect, munmap,
    os_sync_wait_on_address_with_timeout, os_sync_wake_by_address_all, pthread_self, sigaction,
};
use wait::{
    in_flight, learn_reader, member_bit, page_flying, phase_now, sleep_on_wake, wait_complete,
    wake_all,
};

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
    /// A newer frame already tagged the page at the completion and the GPU made no copy of the completed frame's bytes
    /// (the page became a CPU-read result after its flush): the mirror may hold the newer frame's bytes.
    NoCopy,
}

impl SnapWhy {
    /// The number of reasons.
    pub const COUNT: usize = 8;
    /// All reasons, in order.
    pub const ALL: [SnapWhy; Self::COUNT] = [
        SnapWhy::Never,
        SnapWhy::Released,
        SnapWhy::Busy,
        SnapWhy::Unknown,
        SnapWhy::Idle,
        SnapWhy::FlushIdle,
        SnapWhy::Poison,
        SnapWhy::NoCopy,
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
            SnapWhy::NoCopy => "no copy of the frame's bytes",
        }
    }
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

fn now_ns() -> u64 {
    // SAFETY: the call has no preconditions and is safe in a signal handler.
    unsafe { clock_gettime_nsec_np(CLOCK_UPTIME_RAW) }
}

fn pack(state: u32, frame: u64) -> u32 {
    state | (((frame & FRAME_MASK) as u32) << FRAME_SHIFT)
}

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
    /// Results one frame late: the pages of each frame in flight that the GPU copies at the frame's start and end,
    /// oldest first.
    late_copies: VecDeque<LateCopy>,
    /// Results one frame late: CPU blocks that a completion replaced in guest memory with the frame's bytes before a
    /// flush staged them, by page. The next flush stages them.
    carry: BTreeMap<u32, Carry>,
}

/// Results one frame late, exact: the pages of a frame whose bytes game threads read late. The GPU copies them from the
/// mirror after the copy-in at the frame's start and after its last pass, so the completion gets the frame's own bytes
/// (the mirror itself may already hold the next frame's copy-in and writes) and sees which blocks the frame changed.
struct LateCopy {
    frame: u64,
    /// The pages, sorted. Page `pages[i]` is at `i * page` in each copy.
    pages: Vec<u32>,
    /// The host addresses of the copy at the start and at the end; 0 until the encoder registered them.
    start: u64,
    end: u64,
}

impl LateCopy {
    /// The host addresses of page `pg` in the copies at the start and at the end.
    fn of(&self, pg: u32) -> Option<(u64, u64)> {
        if self.start == 0 || self.end == 0 {
            return None;
        }
        let i = self.pages.binary_search(&pg).ok()? as u64 * page();
        Some((self.start + i, self.end + i))
    }
}

/// CPU blocks of a page that a completion replaced with the frame's bytes before a flush staged them: the CPU's bytes
/// and the frame's bytes that replaced them, at their offsets in the page.
struct Carry {
    mask: Mask,
    cpu: Vec<u8>,
    frame: Vec<u8>,
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
    late_copies: VecDeque::new(),
    carry: BTreeMap::new(),
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

/// The page tracker of the process. There is one, because the signal handler needs its table.
#[derive(Clone, Copy, Debug)]
pub struct Pages {
    _private: (),
}

static PAGES: Pages = Pages { _private: () };

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
