//! Events and `WaitSynchronization`.

use crate::handle::{Event, KObject};
use crate::kernel::{Kernel, MAX_WAIT_HANDLES};
use crate::result::{KResult, KResultOr};
use crate::thread::{Thread, Timeout, WaitKind, lock};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

enum Waitable {
    Event(Arc<Event>),
    Thread(Arc<Thread>),
}

impl Waitable {
    fn sync(&self) -> &crate::thread::SyncObject {
        match self {
            Waitable::Event(e) => &e.sync,
            Waitable::Thread(t) => &t.exit,
        }
    }
}

impl Kernel {
    /// `CreateEvent`: the write end and the read end.
    pub fn create_event(&self) -> KResultOr<(u32, u32)> {
        let e = Arc::new(Event {
            sync: Default::default(),
        });
        let w = self.handles.insert(KObject::EventWrite(e.clone()))?;
        match self.handles.insert(KObject::EventRead(e)) {
            Ok(r) => Ok((w, r)),
            Err(err) => {
                // The write end is an event and has nothing to close.
                drop(self.handles.remove(w));
                Err(err)
            }
        }
    }

    /// `SignalEvent`: takes the write end.
    pub fn signal_event(&self, handle: u32) -> KResultOr<()> {
        match self.handles.get(handle) {
            Some(KObject::EventWrite(e)) => {
                e.sync.signal();
                Ok(())
            }
            _ => Err(KResult::INVALID_HANDLE),
        }
    }

    /// `ClearEvent`: takes either end.
    pub fn clear_event(&self, handle: u32) -> KResultOr<()> {
        match self.handles.get(handle) {
            Some(KObject::EventWrite(e) | KObject::EventRead(e)) => {
                e.sync.clear();
                Ok(())
            }
            _ => Err(KResult::INVALID_HANDLE),
        }
    }

    /// `ResetSignal`: takes the read end, and fails when the event is not signalled.
    pub fn reset_signal(&self, handle: u32) -> KResultOr<()> {
        match self.handles.get(handle) {
            Some(KObject::EventRead(e)) if e.sync.reset() => Ok(()),
            Some(KObject::EventRead(_) | KObject::Thread(_)) => Err(KResult::INVALID_STATE),
            _ => Err(KResult::INVALID_HANDLE),
        }
    }

    /// Whether the read end or thread of `handle` is signalled, for tools and tests.
    pub fn is_signaled(&self, handle: u32) -> KResultOr<bool> {
        match self.handles.get(handle) {
            Some(KObject::EventRead(e)) => Ok(e.sync.is_signaled()),
            Some(KObject::Thread(t)) => Ok(t.exit.is_signaled()),
            _ => Err(KResult::INVALID_HANDLE),
        }
    }

    /// `WaitSynchronization`: the index of the first signalled handle.
    pub fn wait_synchronization(
        &self,
        cur: &Arc<Thread>,
        handles: &[u32],
        timeout: Timeout,
    ) -> KResultOr<usize> {
        self.svc_enter(cur);
        if handles.len() > MAX_WAIT_HANDLES {
            return Err(KResult::OUT_OF_RANGE);
        }
        let mut objects = Vec::with_capacity(handles.len());
        for h in handles {
            objects.push(match self.handles.get(*h) {
                Some(KObject::EventRead(e)) => Waitable::Event(e),
                Some(KObject::Thread(t)) => Waitable::Thread(t),
                _ => return Err(KResult::INVALID_HANDLE),
            });
        }
        let deadline = match timeout {
            Timeout::After(d) => Instant::now().checked_add(d),
            _ => None,
        };
        loop {
            if cur.take_cancel() {
                return Err(KResult::CANCELLED);
            }
            cur.parker.prepare();
            *lock(&cur.wait_kind) = WaitKind::Sync;
            let hit = objects.iter().position(|o| o.sync().check_or_register(cur));
            let left = match (timeout, deadline) {
                (Timeout::Immediate, _) => Timeout::Immediate,
                (Timeout::After(_), Some(dl)) => match dl.checked_duration_since(Instant::now()) {
                    Some(l) if !l.is_zero() => Timeout::After(l),
                    _ => Timeout::Immediate,
                },
                _ => Timeout::Infinite,
            };
            let mut timed_out = false;
            if hit.is_none() {
                if left == Timeout::Immediate {
                    timed_out = true;
                } else {
                    timed_out = !self.block_on(cur, left, || true);
                }
            }
            for o in &objects {
                o.sync().unregister(cur);
            }
            *lock(&cur.wait_kind) = WaitKind::None;
            if let Some(i) = hit {
                return Ok(i);
            }
            if timed_out {
                return Err(if cur.take_cancel() {
                    KResult::CANCELLED
                } else {
                    KResult::TIMED_OUT
                });
            }
        }
    }

    /// `CancelSynchronization`: ends the thread's `WaitSynchronization`, or its next one.
    pub fn cancel_synchronization(&self, handle: u32) -> KResultOr<()> {
        let t = self.thread_by_handle(handle)?;
        t.cancel.store(true, Ordering::SeqCst);
        let kind = lock(&t.wait_kind);
        if *kind == WaitKind::Sync {
            t.parker.wake(KResult::CANCELLED);
        }
        Ok(())
    }
}
