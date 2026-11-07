//! The kernel: configuration, threads, events and `WaitSynchronization`. Locks and addresses are in `arbiter`.
//!
//! The kernel has no global lock. A handle lookup holds the table's lock for one step. A wait list is in one shard
//! of a table by address, and an event holds its own lock. A blocked thread waits on its own parker word.

use crate::gate::CoreGates;
use crate::handle::{CURRENT_THREAD, Event, HandleTable, KObject};
use crate::memory::GuestMemory;
use crate::result::{KResult, KResultOr};
use crate::role::{Qos, RoleTable, ThreadInfo, ThreadRole, qos_for_role};
use crate::stats::{StatsSnapshot, format_stats};
use crate::svc::GuestLauncher;
use crate::sys;
use crate::thread::{Thread, ThreadState, Timeout, WaitKind, lock};
use crate::waitq::WaitTable;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

/// The most handles `WaitSynchronization` takes.
pub const MAX_WAIT_HANDLES: usize = 64;
/// The highest priority number.
pub const MAX_PRIORITY: i32 = 63;

/// How the kernel behaves. The title and the family decide most of it.
#[derive(Clone)]
pub struct Config {
    /// The cores of the process, one bit each. A game on the Switch has cores 0 to 2.
    pub core_mask: u64,
    /// The size of the handle table.
    pub handle_capacity: usize,
    /// The roles the title gives its threads.
    pub roles: RoleTable,
    /// The QoS class of a role.
    pub qos_for: fn(ThreadRole) -> Qos,
    /// Whether the kernel sets the QoS class of threads at all.
    pub apply_qos: bool,
    /// The core rules of Horizon. Off, unless the family or the title needs them.
    pub core_rules: bool,
    /// Where the kernel writes its notes.
    pub log: fn(&str),
    /// Makes the body of a guest thread for `CreateThread`. Without it `dispatch_svc` refuses that call.
    pub launcher: Option<Arc<dyn GuestLauncher>>,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            core_mask: 0b111,
            handle_capacity: 4096,
            roles: RoleTable::default(),
            qos_for: qos_for_role,
            apply_qos: true,
            core_rules: false,
            log: |m| eprintln!("nxkit-hos: {m}"),
            launcher: None,
        }
    }
}

/// What a new thread needs.
pub struct ThreadSpec {
    /// The name, empty when the game has not set one.
    pub name: String,
    /// The guest address of the entry function. The role table can match it.
    pub entry: u64,
    /// The Switch priority, 0 to 63.
    pub priority: i32,
    /// The ideal core, or -2 for the process default.
    pub core: i32,
    /// Not used yet. The guest code runs on its own stack and the host thread has a fixed 1 MiB stack.
    pub stack_size: usize,
    /// Runs the guest code. It returns when the guest thread ends.
    pub body: Box<dyn FnOnce() + Send>,
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<Thread>>> = const { RefCell::new(None) };
}

/// The guest thread of the calling host thread.
pub fn current_thread() -> Option<Arc<Thread>> {
    CURRENT.with(|c| c.borrow().clone())
}

/// The Horizon kernel of one process.
pub struct Kernel {
    pub(crate) config: Config,
    pub(crate) mem: Arc<dyn GuestMemory>,
    pub(crate) handles: HandleTable,
    pub(crate) locks: WaitTable,
    pub(crate) condvars: WaitTable,
    pub(crate) addresses: WaitTable,
    pub(crate) gates: CoreGates,
    threads: Mutex<Vec<Arc<Thread>>>,
    next_id: AtomicU64,
    unknown_logged: AtomicBool,
    me: Weak<Kernel>,
}

impl Kernel {
    /// A kernel over a guest memory.
    pub fn new(config: Config, mem: Arc<dyn GuestMemory>) -> Arc<Kernel> {
        Arc::new_cyclic(|me| Kernel {
            handles: HandleTable::new(config.handle_capacity),
            gates: CoreGates::new(config.core_rules),
            config,
            mem,
            locks: WaitTable::new(),
            condvars: WaitTable::new(),
            addresses: WaitTable::new(),
            threads: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(0x51),
            unknown_logged: AtomicBool::new(false),
            me: me.clone(),
        })
    }

    fn check_priority(p: i32) -> KResultOr<()> {
        if (0..=MAX_PRIORITY).contains(&p) {
            Ok(())
        } else {
            Err(KResult::INVALID_PRIORITY)
        }
    }

    fn new_thread(&self, spec: &ThreadSpec, state: ThreadState) -> KResultOr<Arc<Thread>> {
        Self::check_priority(spec.priority)?;
        let core = match spec.core {
            -2 => 0,
            c if (0..64).contains(&c) && self.config.core_mask & 1 << c != 0 => c,
            _ => return Err(KResult::INVALID_CORE_ID),
        };
        let t = Arc::new(Thread::new(
            self.next_id.fetch_add(1, Ordering::Relaxed),
            &spec.name,
            spec.entry,
            spec.priority,
            core,
            self.config.core_mask,
            state,
        ));
        t.handle.store(
            self.handles.insert(KObject::Thread(t.clone()))?,
            Ordering::Relaxed,
        );
        self.assign_role(&t);
        lock(&self.threads).push(t.clone());
        Ok(t)
    }

    /// Registers the calling host thread as a started guest thread, for the host's main thread.
    pub fn attach_current_thread(
        &self,
        name: &str,
        priority: i32,
        core: i32,
    ) -> KResultOr<Arc<Thread>> {
        let spec = ThreadSpec {
            name: name.to_string(),
            entry: 0,
            priority,
            core,
            stack_size: 0,
            body: Box::new(|| {}),
        };
        let t = self.new_thread(&spec, ThreadState::Started)?;
        CURRENT.with(|c| *c.borrow_mut() = Some(t.clone()));
        self.apply_qos(&t);
        self.gates.enter(&t);
        Ok(t)
    }

    /// `CreateThread`: returns the handle. The thread runs after `start_thread`.
    pub fn create_thread(&self, spec: ThreadSpec) -> KResultOr<u32> {
        let body = spec.body;
        let spec = ThreadSpec {
            body: Box::new(|| {}),
            ..spec
        };
        let t = self.new_thread(&spec, ThreadState::Created)?;
        *lock(&t.body) = Some(body);
        Ok(t.handle())
    }

    /// `StartThread`.
    pub fn start_thread(&self, handle: u32) -> KResultOr<()> {
        let t = self.thread_by_handle(handle)?;
        if t.state() != ThreadState::Created {
            return Err(KResult::INVALID_STATE);
        }
        let Some(kernel) = self.me.upgrade() else {
            return Err(KResult::INVALID_STATE);
        };
        t.set_state(ThreadState::Started);
        let body = lock(&t.body).take();
        let name = t.name();
        let th = t.clone();
        let spawned = std::thread::Builder::new()
            .name(if name.is_empty() {
                format!("guest-{:#x}", t.id)
            } else {
                name
            })
            .stack_size(1 << 20)
            .spawn(move || {
                CURRENT.with(|c| *c.borrow_mut() = Some(th.clone()));
                let _exit = ExitGuard(kernel.clone(), th.clone());
                kernel.apply_qos(&th);
                kernel.gates.enter(&th);
                if let Some(body) = body {
                    body();
                }
            });
        if spawned.is_err() {
            t.set_state(ThreadState::Created);
            return Err(KResult::OUT_OF_RESOURCE);
        }
        Ok(())
    }

    /// `ExitThread`. Marks the thread as exited and signals the threads that wait for it. The host's svc gate then
    /// ends the host thread. Calling it twice does nothing.
    pub fn exit_thread(&self, cur: &Arc<Thread>) {
        if cur.state() == ThreadState::Exited {
            return;
        }
        self.gates.leave(cur);
        cur.set_state(ThreadState::Exited);
        cur.exit.signal();
    }

    fn assign_role(&self, t: &Arc<Thread>) {
        let name = t.name();
        let info = ThreadInfo {
            name: &name,
            priority: t.priority(),
            entry: t.entry,
        };
        let role = self.config.roles.classify(&info).unwrap_or_else(|| {
            if !self.unknown_logged.swap(true, Ordering::Relaxed) {
                (self.config.log)(&format!(
                    "thread {name:?} (priority {}, entry {:#x}) matches no role and runs at user-interactive. \
                     This note is written once.",
                    t.priority(),
                    t.entry
                ));
            }
            ThreadRole::Unknown
        });
        t.set_role(role);
        t.qos_pending.store(true, Ordering::Relaxed);
    }

    /// Sets the class of the calling thread. Only a thread can set its own class, so a change from outside is
    /// kept and applied at the thread's next call.
    pub(crate) fn apply_qos(&self, t: &Arc<Thread>) {
        t.qos_pending.store(false, Ordering::Relaxed);
        if !self.config.apply_qos {
            return;
        }
        let want = (self.config.qos_for)(t.role());
        if t.applied_qos() != Some(want) && sys::set_current_qos(want) {
            t.qos_applied.store(want.to_u8(), Ordering::Relaxed);
        }
    }

    /// The start of every blocking call.
    pub(crate) fn svc_enter(&self, cur: &Arc<Thread>) {
        if cur.qos_pending.load(Ordering::Relaxed) {
            self.apply_qos(cur);
        }
    }

    /// Names a thread, which the host learns when the game sets the name. The role is chosen again.
    pub fn set_thread_name(&self, handle: u32, name: &str) -> KResultOr<()> {
        let t = self.thread_by_handle(handle)?;
        *lock(&t.name) = name.to_string();
        self.assign_role(&t);
        if current_thread().is_some_and(|c| Arc::ptr_eq(&c, &t)) {
            self.apply_qos(&t);
        }
        Ok(())
    }

    /// The thread a handle names.
    pub fn thread_by_handle(&self, handle: u32) -> KResultOr<Arc<Thread>> {
        match self.handles.get(handle) {
            Some(KObject::Thread(t)) => Ok(t),
            _ => Err(KResult::INVALID_HANDLE),
        }
    }

    fn resolve_thread(&self, cur: &Arc<Thread>, handle: u32) -> KResultOr<Arc<Thread>> {
        if handle == CURRENT_THREAD {
            Ok(cur.clone())
        } else {
            self.thread_by_handle(handle)
        }
    }

    /// `CloseHandle`.
    pub fn close_handle(&self, handle: u32) -> KResultOr<()> {
        self.handles.remove(handle)
    }

    /// `GetThreadPriority`.
    pub fn get_thread_priority(&self, cur: &Arc<Thread>, handle: u32) -> KResultOr<i32> {
        Ok(self.resolve_thread(cur, handle)?.priority())
    }

    /// `SetThreadPriority`. The QoS class does not change, it follows the role.
    pub fn set_thread_priority(
        &self,
        cur: &Arc<Thread>,
        handle: u32,
        priority: i32,
    ) -> KResultOr<()> {
        Self::check_priority(priority)?;
        let t = self.resolve_thread(cur, handle)?;
        self.gates.set_priority(&t, priority);
        Ok(())
    }

    /// `GetThreadCoreMask`: the ideal core and the mask.
    pub fn get_thread_core_mask(&self, cur: &Arc<Thread>, handle: u32) -> KResultOr<(i32, u64)> {
        let t = self.resolve_thread(cur, handle)?;
        Ok((
            t.ideal_core.load(Ordering::Relaxed),
            t.core_mask.load(Ordering::Relaxed),
        ))
    }

    /// `SetThreadCoreMask`. The ideal core is -1 for none, -2 for the process default and -3 to keep it.
    pub fn set_thread_core_mask(
        &self,
        cur: &Arc<Thread>,
        handle: u32,
        ideal: i32,
        mask: u64,
    ) -> KResultOr<()> {
        let t = self.resolve_thread(cur, handle)?;
        let process = self.config.core_mask;
        let (ideal, mask) = match ideal {
            -2 => (0, 1u64),
            -1 => (-1, process),
            c => {
                let c = if c == -3 {
                    t.ideal_core.load(Ordering::Relaxed)
                } else {
                    c
                };
                if c >= 0 && (c >= 64 || process & 1 << c == 0) {
                    return Err(KResult::INVALID_CORE_ID);
                }
                if mask == 0 || mask | process != process {
                    return Err(KResult::INVALID_CORE_ID);
                }
                if c >= 0 && mask & 1 << c == 0 {
                    return Err(KResult::INVALID_COMBINATION);
                }
                (c, mask)
            }
        };
        t.ideal_core.store(ideal, Ordering::Relaxed);
        t.core_mask.store(mask, Ordering::Relaxed);
        Ok(())
    }

    /// `GetCurrentProcessorNumber`.
    pub fn current_processor_number(&self, cur: &Arc<Thread>) -> u32 {
        cur.core() as u32
    }

    /// `GetThreadId`.
    pub fn get_thread_id(&self, cur: &Arc<Thread>, handle: u32) -> KResultOr<u64> {
        Ok(self.resolve_thread(cur, handle)?.id)
    }

    /// `SleepThread`. Zero and the negative values are the three kinds of yield.
    pub fn sleep_thread(&self, cur: &Arc<Thread>, ns: i64) {
        self.svc_enter(cur);
        if ns <= 0 {
            std::thread::yield_now();
            return;
        }
        cur.parker.prepare();
        self.block_on(cur, Timeout::from_ns(ns), || true);
    }

    /// Blocks `cur` on its parker. `on_timeout` runs when the timeout ends and says whether the thread took
    /// itself out of its wait list. If it says no, a waker has taken the thread and the wake is on its way.
    /// Returns true when a waker ended the wait.
    pub(crate) fn block_on(
        &self,
        cur: &Arc<Thread>,
        timeout: Timeout,
        on_timeout: impl FnOnce() -> bool,
    ) -> bool {
        self.gates.leave(cur);
        let t0 = Instant::now();
        cur.stats.blocked.store(true, Ordering::SeqCst);
        let mut signaled = cur.parker.wait(timeout);
        if !signaled && !on_timeout() {
            cur.parker.wait(Timeout::Infinite);
            signaled = true;
        }
        cur.stats.blocked.store(false, Ordering::SeqCst);
        let ns = u64::try_from(t0.elapsed().as_nanos()).unwrap_or(u64::MAX);
        cur.stats.add_wait(ns, signaled);
        self.gates.enter(cur);
        signaled
    }

    /// `CreateEvent`: the write end and the read end.
    pub fn create_event(&self) -> KResultOr<(u32, u32)> {
        let e = Arc::new(Event {
            sync: Default::default(),
        });
        let w = self.handles.insert(KObject::EventWrite(e.clone()))?;
        match self.handles.insert(KObject::EventRead(e)) {
            Ok(r) => Ok((w, r)),
            Err(err) => {
                let _ = self.handles.remove(w);
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

    /// The counters of every thread.
    pub fn stats(&self) -> Vec<StatsSnapshot> {
        lock(&self.threads)
            .iter()
            .map(|t| StatsSnapshot::of(t.id, t.name(), t.role(), &t.stats))
            .collect()
    }

    /// How many threads are parked now in a lock, condition variable or address wait.
    pub fn blocked_threads(&self) -> usize {
        lock(&self.threads)
            .iter()
            .filter(|t| t.stats.blocked.load(Ordering::SeqCst))
            .count()
    }

    /// The counters as a table.
    pub fn dump_stats(&self) -> String {
        format_stats(&self.stats())
    }
}

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

/// Marks the thread as exited when its body returns or panics.
struct ExitGuard(Arc<Kernel>, Arc<Thread>);

impl Drop for ExitGuard {
    fn drop(&mut self) {
        self.0.exit_thread(&self.1);
    }
}
