//! Guest threads, the parker a blocked thread waits on, and the objects a thread can wait for.

use crate::lend::Lend;
use crate::result::KResult;
use crate::role::{Qos, ThreadRole};
use crate::stats::ThreadStats;
use crate::sys::{self, WaitStatus};
use std::sync::atomic::{
    AtomicBool, AtomicI32, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering,
};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// How long a call may block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Timeout {
    /// Do not block.
    Immediate,
    /// Block until signalled.
    Infinite,
    /// Block for at most this long.
    After(Duration),
}

impl Timeout {
    /// The svc convention: below zero waits for ever, zero does not wait.
    pub fn from_ns(ns: i64) -> Timeout {
        match ns {
            n if n < 0 => Timeout::Infinite,
            0 => Timeout::Immediate,
            n => Timeout::After(Duration::from_nanos(n as u64)),
        }
    }
}

/// The word one thread blocks on. A waker stores a result and then wakes it, so a wake that comes before the wait
/// is not lost.
pub(crate) struct Parker {
    state: AtomicU32,
    result: AtomicU32,
}

impl Parker {
    fn new() -> Parker {
        Parker {
            state: AtomicU32::new(0),
            result: AtomicU32::new(0),
        }
    }

    /// Clears the wake. It comes before the thread joins any wait list.
    pub(crate) fn prepare(&self) {
        self.state.store(0, Ordering::SeqCst);
    }

    pub(crate) fn wake(&self, result: KResult) {
        self.result.store(result.raw(), Ordering::Relaxed);
        self.state.store(1, Ordering::Release);
        sys::wake_one(&self.state);
    }

    pub(crate) fn result(&self) -> KResult {
        KResult(self.result.load(Ordering::Relaxed))
    }

    /// True when woken, false when the timeout ended first.
    pub(crate) fn wait(&self, timeout: Timeout) -> bool {
        let deadline = match timeout {
            Timeout::After(d) => Instant::now().checked_add(d),
            _ => None,
        };
        loop {
            if self.state.load(Ordering::Acquire) == 1 {
                return true;
            }
            let left = match (timeout, deadline) {
                (Timeout::Immediate, _) => return false,
                (Timeout::After(_), Some(dl)) => match dl.checked_duration_since(Instant::now()) {
                    Some(l) if !l.is_zero() => Some(l),
                    _ => return self.state.load(Ordering::Acquire) == 1,
                },
                _ => None,
            };
            if sys::wait_u32(&self.state, 0, left) == WaitStatus::TimedOut
                && self.state.load(Ordering::Acquire) != 1
            {
                return false;
            }
        }
    }
}

#[derive(Default)]
struct SyncInner {
    signaled: bool,
    waiters: Vec<Arc<Thread>>,
}

/// The state of a `KSynchronizationObject`: signalled or not, and the threads in `WaitSynchronization` on it.
#[derive(Default)]
pub(crate) struct SyncObject {
    inner: Mutex<SyncInner>,
}

impl SyncObject {
    /// Signals under the object's lock. A waiter leaves the list under the same lock, so a wake never reaches a
    /// thread that has moved on to another wait.
    pub(crate) fn signal(&self) {
        let mut g = lock(&self.inner);
        g.signaled = true;
        for w in &g.waiters {
            w.parker.wake(KResult::SUCCESS);
        }
    }

    pub(crate) fn clear(&self) {
        lock(&self.inner).signaled = false;
    }

    /// Clears the signal. False when it was not set.
    pub(crate) fn reset(&self) -> bool {
        std::mem::take(&mut lock(&self.inner).signaled)
    }

    pub(crate) fn is_signaled(&self) -> bool {
        lock(&self.inner).signaled
    }

    /// True when signalled. Otherwise joins the waiters, in the same step.
    pub(crate) fn check_or_register(&self, t: &Arc<Thread>) -> bool {
        let mut g = lock(&self.inner);
        if g.signaled {
            return true;
        }
        if !g.waiters.iter().any(|w| Arc::ptr_eq(w, t)) {
            g.waiters.push(t.clone());
        }
        false
    }

    pub(crate) fn unregister(&self, t: &Arc<Thread>) {
        lock(&self.inner).waiters.retain(|w| !Arc::ptr_eq(w, t));
    }
}

/// What a thread is waiting in, so that a cancel wakes only a synchronization wait.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum WaitKind {
    None,
    Sync,
}

/// The run state of a thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadState {
    /// Created, `StartThread` not called.
    Created,
    /// Running or blocked.
    Started,
    /// Exited. Its exit signals waiters.
    Exited,
}

/// A guest thread. One guest thread is one host thread.
pub struct Thread {
    pub(crate) id: u64,
    pub(crate) handle: AtomicU32,
    pub(crate) name: Mutex<String>,
    pub(crate) entry: u64,
    pub(crate) priority: AtomicI32,
    pub(crate) ideal_core: AtomicI32,
    pub(crate) core_mask: AtomicU64,
    role: AtomicU8,
    pub(crate) qos_applied: AtomicU8,
    pub(crate) qos_pending: AtomicBool,
    pub(crate) lend: Lend,
    pub(crate) tls_slot: AtomicU64,
    pub(crate) stack_size: AtomicUsize,
    state: AtomicU8,
    pub(crate) exit: SyncObject,
    pub(crate) parker: Parker,
    pub(crate) wait_kind: Mutex<WaitKind>,
    pub(crate) cancel: AtomicBool,
    pub(crate) stats: ThreadStats,
    pub(crate) gate_slot: Mutex<Option<(usize, i32)>>,
    pub(crate) body: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Thread {
    pub(crate) fn new(
        id: u64,
        name: &str,
        entry: u64,
        priority: i32,
        ideal_core: i32,
        core_mask: u64,
        state: ThreadState,
    ) -> Thread {
        Thread {
            id,
            handle: AtomicU32::new(0),
            name: Mutex::new(name.to_string()),
            entry,
            priority: AtomicI32::new(priority),
            ideal_core: AtomicI32::new(ideal_core),
            core_mask: AtomicU64::new(core_mask),
            role: AtomicU8::new(ThreadRole::Unknown.to_u8()),
            qos_applied: AtomicU8::new(0),
            qos_pending: AtomicBool::new(false),
            lend: Lend::default(),
            tls_slot: AtomicU64::new(0),
            stack_size: AtomicUsize::new(0),
            state: AtomicU8::new(state as u8),
            exit: SyncObject::default(),
            parker: Parker::new(),
            wait_kind: Mutex::new(WaitKind::None),
            cancel: AtomicBool::new(false),
            stats: ThreadStats::default(),
            gate_slot: Mutex::new(None),
            body: Mutex::new(None),
        }
    }

    /// The address of the thread's TLS block (`tpidr_el0`), or zero when the kernel does not model thread memory.
    pub fn tls(&self) -> u64 {
        match self.tls_slot.load(Ordering::Acquire) {
            0 => 0,
            slot => slot + crate::vm::TLS_OFFSET,
        }
    }

    /// The address of the thread-local region (`tpidrro_el0`) with the IPC buffer in its first 0x100 bytes, or zero
    /// when the kernel does not model thread memory.
    pub fn tlr(&self) -> u64 {
        match self.tls_slot.load(Ordering::Acquire) {
            0 => 0,
            slot => slot + crate::vm::TLR_OFFSET,
        }
    }

    /// The thread id the guest reads with `GetThreadId`.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The handle of the thread in the process's handle table.
    pub fn handle(&self) -> u32 {
        self.handle.load(Ordering::Relaxed)
    }

    /// The name the host knows, empty when none was set.
    pub fn name(&self) -> String {
        lock(&self.name).clone()
    }

    /// The Switch priority, 0 is the highest.
    pub fn priority(&self) -> i32 {
        self.priority.load(Ordering::Relaxed)
    }

    /// The role of the thread.
    pub fn role(&self) -> ThreadRole {
        ThreadRole::from_u8(self.role.load(Ordering::Relaxed))
    }

    pub(crate) fn set_role(&self, role: ThreadRole) {
        self.role.store(role.to_u8(), Ordering::Relaxed);
    }

    /// The run state.
    pub fn state(&self) -> ThreadState {
        match self.state.load(Ordering::Acquire) {
            0 => ThreadState::Created,
            1 => ThreadState::Started,
            _ => ThreadState::Exited,
        }
    }

    pub(crate) fn set_state(&self, s: ThreadState) {
        self.state.store(s as u8, Ordering::Release);
    }

    /// The core the thread belongs to for the core rules: the ideal core, else the lowest core of the mask.
    pub(crate) fn core(&self) -> usize {
        let ideal = self.ideal_core.load(Ordering::Relaxed);
        if ideal >= 0 {
            return ideal as usize;
        }
        self.core_mask
            .load(Ordering::Relaxed)
            .trailing_zeros()
            .min(3) as usize
    }

    pub(crate) fn applied_qos(&self) -> Option<Qos> {
        match self.qos_applied.load(Ordering::Relaxed) {
            1 => Some(Qos::UserInteractive),
            2 => Some(Qos::UserInitiated),
            3 => Some(Qos::Utility),
            4 => Some(Qos::Background),
            _ => None,
        }
    }

    pub(crate) fn take_cancel(&self) -> bool {
        self.cancel.swap(false, Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_follows_the_svc_convention() {
        assert_eq!(Timeout::from_ns(-1), Timeout::Infinite);
        assert_eq!(Timeout::from_ns(0), Timeout::Immediate);
        assert_eq!(Timeout::from_ns(5), Timeout::After(Duration::from_nanos(5)));
    }

    #[test]
    fn a_wake_before_the_wait_is_kept_and_a_timeout_reports_false() {
        let p = Parker::new();
        p.wake(KResult::CANCELLED);
        assert!(p.wait(Timeout::Infinite));
        assert_eq!(p.result(), KResult::CANCELLED);
        p.prepare();
        let t0 = Instant::now();
        assert!(!p.wait(Timeout::After(Duration::from_millis(20))));
        assert!(t0.elapsed() >= Duration::from_millis(15));
        assert!(!p.wait(Timeout::Immediate));
    }
}
