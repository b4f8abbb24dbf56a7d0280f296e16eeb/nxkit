//! The descriptors that work without a network: event descriptors, `Poll`, `Select` and `Close`. Source: nxnative
//! `dm/host/hle_net.c`, the socket section (`efd_*`, `dm_sk_poll`, `dm_sk_select`).
//!
//! The host creates no socket, so every call that needs a peer fails with -1 and the errno `ENETDOWN` (50, the
//! FreeBSD value of Horizon). `EventFd` is a local descriptor of the bsd service. It works with no network and is real
//! here, because a game wakes its pollers with one. The descriptors are the numbers 0x4000 to 0x403f. A counter has
//! the flags of Horizon: bit 0 reads one at a time (semaphore), bits 1 and 2 never block.
//!
//! The counters are atomics. A read or write that does not block touches no lock. A call that has to wait sleeps on a
//! condition variable that the writer wakes, and a writer takes the lock only when a waiter exists. nxnative takes
//! one global lock on every call and wakes sleepers in slices of 100 ms. A wait with no descriptor of the host is a
//! plain sleep. nxnative uses the guest-aware sleep of its thread layer here.

use super::{get_i32, get_u16, put_u16};
use crate::util::lock;
use std::cell::Cell;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

pub(super) const ENETDOWN: i32 = 50;
const EMFILE: i32 = 24;
const EBADF: i32 = 9;
const EAGAIN: i32 = 35;
const BASE: i32 = 0x4000;
const MAX: usize = 64;
const IN_USE: u32 = 0x8000_0000;
const POLLIN: u16 = 1;
const POLLOUT: u16 = 4;
const POLLRDNORM: u16 = 0x40;
const POLLWRNORM: u16 = 4;
const POLLNVAL: u16 = 0x20;
/// The longest wait of one `Poll` call. An endless wait returns after this, as in nxnative, so the thread comes back to
/// the host.
const IDLE_MS: i64 = 100;

thread_local! {
    static ERRNO: Cell<i32> = const { Cell::new(ENETDOWN) };
}

/// The errno of the calling thread.
pub(super) fn errno() -> i32 {
    ERRNO.with(Cell::get)
}

/// Sets the errno of the calling thread.
pub(super) fn set_errno(e: i32) {
    ERRNO.with(|c| c.set(e));
}

/// Fails with -1 and `ENETDOWN`.
pub(super) fn down() -> i32 {
    set_errno(ENETDOWN);
    -1
}

struct Slot {
    /// 0 when free, else `IN_USE` and the flags of the descriptor.
    flags: AtomicU32,
    count: AtomicU64,
}

static SLOTS: [Slot; MAX] = [const { Slot { flags: AtomicU32::new(0), count: AtomicU64::new(0) } }; MAX];
static WAITERS: AtomicU32 = AtomicU32::new(0);
static PARK: Mutex<()> = Mutex::new(());
static WAKE: Condvar = Condvar::new();

fn slot(fd: i32) -> Option<&'static Slot> {
    let i = usize::try_from(fd.checked_sub(BASE)?).ok()?;
    let s = SLOTS.get(i)?;
    (s.flags.load(Ordering::Acquire) != 0).then_some(s)
}

/// Wakes the sleepers. A writer that finds no waiter takes no lock. A waiter counts itself in before it checks its
/// condition under the lock, so a wake-up cannot fall between the check and the sleep.
fn wake() {
    if WAITERS.load(Ordering::SeqCst) > 0 {
        let _guard = lock(&PARK);
        WAKE.notify_all();
    }
}

/// Sleeps until `ready` holds or `until` passes. Gives whether `ready` held.
fn park(mut ready: impl FnMut() -> bool, until: Option<Instant>) -> bool {
    WAITERS.fetch_add(1, Ordering::SeqCst);
    let mut guard = lock(&PARK);
    let held = loop {
        if ready() {
            break true;
        }
        guard = match until {
            None => WAKE.wait(guard).unwrap_or_else(PoisonError::into_inner),
            Some(t) => {
                let now = Instant::now();
                if now >= t {
                    break false;
                }
                WAKE.wait_timeout(guard, t - now).unwrap_or_else(PoisonError::into_inner).0
            }
        };
    };
    drop(guard);
    WAITERS.fetch_sub(1, Ordering::SeqCst);
    held
}

/// Takes the value of the counter: one in semaphore mode, else everything.
fn take(s: &Slot) -> Option<u64> {
    let semaphore = s.flags.load(Ordering::Acquire) & 1 != 0;
    let mut c = s.count.load(Ordering::Acquire);
    while c != 0 {
        let next = if semaphore { c - 1 } else { 0 };
        match s.count.compare_exchange_weak(c, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return Some(if semaphore { 1 } else { c }),
            Err(now) => c = now,
        }
    }
    None
}

/// `EventFd(unsigned long value, EventFdFlags)`.
pub(super) extern "C" fn event_fd(value: u64, flags: u32) -> i32 {
    for (i, s) in SLOTS.iter().enumerate() {
        if s.flags.compare_exchange(0, IN_USE | flags, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            s.count.store(value, Ordering::Release);
            return BASE + i as i32;
        }
    }
    super::log(&format!("net: more than {MAX} event descriptors"));
    set_errno(EMFILE);
    -1
}

/// `EventFdRead(int, unsigned long*)`.
pub(super) extern "C" fn event_fd_read(fd: u32, out: u64) -> i32 {
    let Some(s) = slot(fd as i32) else {
        set_errno(EBADF);
        return -1;
    };
    let mut got = take(s);
    if got.is_none() {
        if s.flags.load(Ordering::Acquire) & 6 != 0 {
            set_errno(EAGAIN);
            return -1;
        }
        let used = || s.flags.load(Ordering::Acquire) != 0;
        park(
            || {
                got = take(s);
                got.is_some() || !used()
            },
            None,
        );
    }
    match got {
        Some(v) => {
            super::put_u64(out, v);
            0
        }
        None => {
            set_errno(EBADF);
            -1
        }
    }
}

/// `EventFdWrite(int, unsigned long)`.
pub(super) extern "C" fn event_fd_write(fd: u32, value: u64) -> i32 {
    let Some(s) = slot(fd as i32) else {
        set_errno(EBADF);
        return -1;
    };
    s.count.fetch_add(value, Ordering::AcqRel);
    wake();
    0
}

/// `Close(int)` and `nnsocketClose`: closes an event descriptor, and fails for any other number.
pub(super) extern "C" fn close(fd: u32) -> i32 {
    match slot(fd as i32) {
        Some(s) => {
            s.flags.store(0, Ordering::Release);
            wake();
            0
        }
        None => down(),
    }
}

/// The `revents` of one entry, and whether it counts as ready.
fn revents(fd: i32, events: u16) -> (u16, bool) {
    if fd < 0 || !(BASE..BASE + MAX as i32).contains(&fd) {
        return (0, false);
    }
    let Some(s) = slot(fd) else {
        return (POLLNVAL, true);
    };
    let mut r = 0;
    if s.count.load(Ordering::Acquire) != 0 {
        r |= events & (POLLIN | POLLRDNORM);
    }
    r |= events & (POLLOUT | POLLWRNORM);
    (r, r != 0)
}

/// Fills the `revents` of `n` entries `{i32 fd, i16 events, i16 revents}` and gives how many are ready.
fn scan(fds: u64, n: u64) -> i32 {
    let mut ready = 0;
    for i in 0..n {
        let at = fds + 8 * i;
        let (r, hit) = revents(get_i32(at), get_u16(at + 4));
        put_u16(at + 6, r);
        ready += i32::from(hit);
    }
    ready
}

fn idle(ms: i64) -> i32 {
    std::thread::sleep(Duration::from_millis(u64::try_from(if ms < 0 { IDLE_MS } else { ms }).unwrap_or(0)));
    0
}

/// `Poll(PollFd*, size_t, int)`, `nnsocketPoll` and the C library `poll`.
///
/// A descriptor that the host never made has nothing to say, and the wait is spent in full. An event descriptor is ready
/// when its counter is not zero. A wait with no timeout ends after 100 ms.
pub(super) extern "C" fn poll(fds: u64, n: u64, ms: u32) -> i32 {
    let ms = i64::from(ms as i32);
    let n = n.min(0x1_0000);
    let any = (0..n).any(|i| slot(get_i32(fds + 8 * i)).is_some());
    if !any {
        for i in 0..n {
            put_u16(fds + 8 * i + 6, 0);
        }
        return idle(ms);
    }
    let until = Instant::now() + Duration::from_millis(u64::try_from(if ms < 0 { IDLE_MS } else { ms }).unwrap_or(0));
    let mut ready = scan(fds, n);
    if ready == 0 {
        park(
            || {
                ready = scan(fds, n);
                ready > 0
            },
            Some(until),
        );
    }
    ready
}

/// `Select(int, FdSet*, FdSet*, FdSet*, TimeVal*)`: an fd set with a descriptor fails, else the call idles for the
/// time. An `FdSet` is 1024 bits, a `TimeVal` is `{i64 seconds, i64 microseconds}`.
pub(super) extern "C" fn select(nfds: u32, r: u64, w: u64, e: u64, tv: u64) -> i32 {
    let nfds = (nfds as i32).clamp(0, 1024) as u64;
    for set in [r, w, e] {
        if set == 0 {
            continue;
        }
        for fd in 0..nfds {
            if super::get_u64(set + (fd >> 6) * 8) >> (fd & 63) & 1 != 0 {
                return down();
            }
        }
    }
    if tv == 0 {
        return idle(-1);
    }
    idle(super::get_i64(tv) * 1000 + super::get_i64(tv + 8) / 1000)
}
