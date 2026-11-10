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

use std::collections::VecDeque;
use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};
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

unsafe extern "C" {
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
    pcs: [[(AtomicU64, AtomicU64); PC_SLOTS]; 3],
    pc_overflow: AtomicU64,
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
    synced: AtomicU64::new(0),
    writer_waits: AtomicU64::new(0),
    writer_wait_ns: AtomicU64::new(0),
    internal_opens: AtomicU64::new(0),
    wait_timeouts: AtomicU64::new(0),
    member_bits: AtomicPtr::new(std::ptr::null_mut()),
    member_gen: AtomicU64::new(0),
    pcs: [PC_TABLE; 3],
    pc_overflow: AtomicU64::new(0),
};

/// What only the holders of the mutex touch.
struct Ctrl {
    /// The runs of pages (first, end) that each frame in flight protected, oldest first.
    frames: VecDeque<(u64, Vec<(u32, u32)>)>,
    /// The member bitmap at a generation, for [`Pages::member_snapshot`].
    snap: Option<(u64, std::sync::Arc<Vec<u64>>)>,
}

static CTRL: Mutex<Ctrl> = Mutex::new(Ctrl {
    frames: VecDeque::new(),
    snap: None,
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
    if k > 2 || pc == 0 {
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

/// True when `frame` (28 bits) is a frame whose completion has not been reported.
fn in_flight(frame: u32) -> bool {
    u64::from(frame) > SHARED.completed.load(Ordering::Acquire) & FRAME_MASK
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

/// Waits until the completion of `frame` is reported. Returns true when the wait ended at the timeout.
fn wait_complete(frame: u32) -> bool {
    let t0 = now_ns();
    let mut waited = false;
    let mut timed_out = false;
    loop {
        let seen = SHARED.wake.load(Ordering::Acquire);
        if !in_flight(frame) {
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
        SHARED.writer_waits.fetch_add(1, Ordering::Relaxed);
        SHARED
            .writer_wait_ns
            .fetch_add(now_ns().saturating_sub(t0), Ordering::Relaxed);
    }
    if timed_out {
        SHARED.wait_timeouts.fetch_add(1, Ordering::Relaxed);
    }
    timed_out
}

/// Copies the mirror page `pg` into guest memory. The guest page must be writable.
fn copy_from_mirror(pg: usize, mirror: u64) {
    let p = page();
    let base = SHARED.base.load(Ordering::Acquire);
    // SAFETY: the guest page is whole host page inside the range that `attach` named and the caller made it writable;
    // the mirror page at the same offset lies in the reservation of the mirror, which is readable while the page is a
    // member. The two regions are different mappings.
    unsafe {
        std::ptr::copy_nonoverlapping(
            (mirror + pg as u64 * p) as *const u8,
            (base + pg as u64 * p) as *mut u8,
            p as usize,
        );
    }
    SHARED.synced.fetch_add(1, Ordering::Relaxed);
}

/// Serves a fault on the tracked page `pg` when there is a mirror. A page tagged with a frame holds stale bytes: the
/// thread waits for the frame to complete (an internal thread does not wait), copies the page from the mirror and opens
/// it. Atomics, `mprotect`, `memcpy` and the wake word; no lock, no allocation.
fn serve_mirrored(pg: usize, w: &AtomicU32, write: bool, internal: bool, mirror: u64) -> FaultKind {
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
                let flying = in_flight(frame) && !gave_up;
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
                    gave_up = wait_complete(frame);
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
                    copy_from_mirror(pg, mirror);
                }
                if flying {
                    SHARED.internal_opens.fetch_add(1, Ordering::Relaxed);
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
            on_tracked(pg, w, pc, write);
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

/// Serves a fault on the tracked page `pg`. Atomics, `mprotect` and nothing else.
fn on_tracked(pg: usize, w: &AtomicU32, pc: u64, write: bool) {
    let internal = is_internal();
    let origin = origin_of(pc, internal);
    let mirror = SHARED.mirror.load(Ordering::Acquire);
    if mirror != 0 {
        let kind = serve_mirrored(pg, w, write, internal, mirror);
        SHARED.faults[kind as usize][origin as usize].fetch_add(1, Ordering::Relaxed);
        if !internal {
            count_pc(kind, pc);
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

/// Receives the guest pages that the CPU wrote, to carry them into the mirror on the GPU.
pub trait Stager {
    /// At most `max_pages` pages follow.
    fn begin(&mut self, max_pages: usize);
    /// The bytes of the guest page `page`. A page that is staged twice keeps the later bytes.
    fn stage(&mut self, page: u32, bytes: &[u8]);
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
        SHARED.members.store(0, Ordering::Release);
        SHARED.completed.store(0, Ordering::Release);
        SHARED.mirror.store(0, Ordering::Release);
        SHARED.npages.store(0, Ordering::Release);
        SHARED.member_bits.store(bits, Ordering::Release);
        SHARED.member_gen.fetch_add(1, Ordering::AcqRel);
        SHARED.table.store(table, Ordering::Release);
        SHARED.base.store(b, Ordering::Release);
        SHARED.len.store(len, Ordering::Release);
        SHARED.npages.store(n, Ordering::Release);
        SHARED.log_n.store(0, Ordering::Release);
        {
            let mut c = ctrl();
            c.frames.clear();
            c.snap = None;
        }
        install_handler();
        nxkit_abi::guest::set_guard_hook(hook);
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

    /// The GPU finished frame `frame` (and, by the order of the queue, every frame before it). Its pages that are still
    /// protected for it go to clean and read only. It runs on the thread of the Metal completion handler.
    pub fn complete(&self, frame: u64) {
        SHARED.completed.fetch_max(frame, Ordering::AcqRel);
        if SHARED.mirror.load(Ordering::Acquire) != 0 {
            // With a mirror the pages stay tagged: the guest bytes are stale until a CPU access copies them. Pages that
            // a host thread opened for reading while the frame ran lose their access again.
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
                close_opened(f, &runs);
            }
            wake_all();
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
                        let k = serve_mirrored(p as usize, w, write, internal, mirror);
                        SHARED.faults[k as usize][FaultOrigin::Host as usize]
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    OPEN if write => {
                        let k = serve_mirrored(p as usize, w, write, internal, mirror);
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
        let calls0 = SHARED.mprotects.load(Ordering::Relaxed);
        let mut c = ctrl();
        let p = page();
        let base = SHARED.base.load(Ordering::Acquire);
        let runs_w = span_runs(written);
        let mut all: Vec<(u64, u64)> = members.to_vec();
        all.extend_from_slice(written);
        let runs_m = span_runs(&all);
        // 1. New members.
        let mut newly: Vec<u32> = Vec::new();
        for &(s, e) in &runs_m {
            for pg in s..e {
                if let Some(w) = word(pg as usize)
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
        stager.begin(dirty.len() + 64);
        let mut back: Vec<u32> = Vec::with_capacity(dirty.len());
        for pg in dirty {
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
                stager.stage(pg, bytes);
            }
        };
        for (s, e) in runs_of(&back) {
            protect_pages(s, e, PROT_READ, true);
            stage_run(stager, s, e);
        }
        st.staged = back.len() as u32;
        // 3. The pages of the frame.
        for &(s, e) in &runs_w {
            for pg in s..e {
                let Some(w) = word(pg as usize) else { continue };
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
            }
            // The pages lose all access first: from here a thread that touches a page faults, finds it tagged and
            // waits for the frame and copies the mirror. Only then is the pattern written, one page at a time, so a page
            // is open and holds the pattern for a few microseconds, not for the time of the whole run.
            protect_pages(s, e, PROT_NONE, true);
            if let Some(pattern) = poison {
                for pg in s..e {
                    protect_pages(pg, pg + 1, PROT_RW, true);
                    // SAFETY: the page is writable now and lies in guest memory.
                    unsafe {
                        std::slice::from_raw_parts_mut((base + u64::from(pg) * p) as *mut u32, (p / 4) as usize)
                            .fill(pattern);
                    }
                    protect_pages(pg, pg + 1, PROT_NONE, true);
                }
            }
            st.written += e - s;
        }
        st.written_runs = runs_w.len() as u32;
        if !runs_w.is_empty() {
            c.frames.push_back((frame, runs_w));
        }
        while c.frames.len() > MAX_FRAMES {
            c.frames.pop_front();
        }
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
        }
    }

    /// The `n` program counters that faulted most for `kind` (only the kinds with a table: a clean write, a read and a
    /// write in flight), most first, with their counts. The tables are emptied.
    pub fn take_top_pcs(&self, kind: FaultKind, n: usize) -> Vec<(u64, u64)> {
        let k = kind as usize;
        if k > 2 {
            return Vec::new();
        }
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
    }

    impl Stager for Collect {
        fn begin(&mut self, _max: usize) {}

        fn stage(&mut self, page: u32, bytes: &[u8]) {
            self.pages.retain(|p| p.0 != page);
            self.pages.push((page, bytes.to_vec()));
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
    fn a_write_in_flight_waits_then_the_cpu_write_lands_on_the_gpus_bytes() {
        let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (m, mir, pg) = mirrored(4);
        let mut st = Collect::default();
        let _ = pg.flush_mirrored(3, &[], &[(m.addr(0), 1)], None, &mut st);
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
    fn runs_merge_and_the_lookup_finds_them() {
        assert_eq!(runs_of(&[1, 2, 3, 7, 8, 12]), vec![(1, 4), (7, 9), (12, 13)]);
        let r = vec![(1, 4), (7, 9)];
        assert!(in_runs(&r, 1) && in_runs(&r, 3) && in_runs(&r, 8));
        assert!(!in_runs(&r, 0) && !in_runs(&r, 4) && !in_runs(&r, 9));
    }
}
