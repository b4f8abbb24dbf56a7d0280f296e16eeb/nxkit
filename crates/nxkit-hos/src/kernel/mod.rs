//! The kernel: configuration, threads, events and `WaitSynchronization`. Locks and addresses are in `arbiter`.
//!
//! The kernel has no global lock. A handle lookup holds the table's lock for one step. A wait list is in one shard
//! of a table by address, and an event holds its own lock. A blocked thread waits on its own parker word.

use crate::gate::CoreGates;
use crate::handle::{CURRENT_THREAD, HandleTable, KObject};
use crate::lend::LendGuard;
use crate::memory::GuestMemory;
use crate::result::{KResult, KResultOr};
use crate::role::{Qos, RoleTable, ThreadInfo, ThreadRole, qos_for_role};
use crate::stats::{StatsSnapshot, format_stats};
use crate::svc::GuestLauncher;
use crate::sys;
use crate::sysevent::SystemEvents;
use crate::thread::{Thread, ThreadState, Timeout, lock};
use crate::vm::{MemoryConfig, Vm};
use crate::waitq::WaitTable;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Instant;

mod spawn;

/// The most handles `WaitSynchronization` takes.
pub const MAX_WAIT_HANDLES: usize = 64;
/// The highest priority number.
pub const MAX_PRIORITY: i32 = 63;

/// The main thread of the process, as the NPDM of the title describes it (`main_thread` in `game.json`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MainThread {
    /// The Switch priority, 0 to 63.
    pub priority: i32,
    /// The core of the thread.
    pub core: i32,
    /// The stack size in bytes.
    pub stack_size: u64,
}

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
    /// Priority, core and stack size of the main thread. `attach_main_thread` needs it. There is no default,
    /// because a guessed priority gave the main thread the wrong place among the game's threads.
    pub main_thread: Option<MainThread>,
    /// The guest's memory. Without it the memory, IPC and `GetInfo` svcs are not answered.
    pub memory: Option<MemoryConfig>,
    /// Ends the process with an exit code, for `ExitProcess`, `Break` and `ReturnFromException`.
    pub process_exit: fn(u32),
    /// The process id that `GetProcessId` returns.
    pub process_id: u64,
    /// Told the value of each handle that `CloseHandle` closed, after the kernel dropped it.
    pub on_close: Option<fn(u32)>,
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
            main_thread: None,
            memory: None,
            process_exit: |code| {
                eprintln!("nxkit-hos: the process ends with code {code:#x}");
                std::process::exit(code as i32)
            },
            // Unverified: the process id of a title is not read anywhere. The SDK only passes it back.
            process_id: 0x80,
            on_close: None,
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
    /// The stack of the host thread in bytes. Zero takes the default of 1 MiB. The size is rounded up to whole pages
    /// and is at least 64 KiB.
    pub stack_size: usize,
    /// Runs the guest code. It returns when the guest thread ends.
    pub body: Box<dyn FnOnce() + Send>,
}

/// The host stack for a requested size: the default for zero, otherwise at least 64 KiB in whole 16 KiB pages.
fn stack_size(requested: usize) -> usize {
    const PAGE: usize = 16 << 10;
    match requested {
        0 => 1 << 20,
        n => n.max(64 << 10).div_ceil(PAGE).saturating_mul(PAGE),
    }
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
    pub(crate) threads: Mutex<Vec<Arc<Thread>>>,
    next_id: AtomicU64,
    unknown_logged: AtomicBool,
    me: Weak<Kernel>,
    pub(crate) vm: Option<Vm>,
    pub(crate) resource_limit: OnceLock<u32>,
    pub(crate) system_events: SystemEvents,
    /// The threads by the lock tag they gave when they waited, for tags that are not kernel handles (see
    /// [`crate::handle::is_handle`]).
    pub(crate) tags: Mutex<std::collections::HashMap<u32, Weak<Thread>>>,
}

impl Kernel {
    /// A kernel over a guest memory. A memory configuration that does not fit is logged and the kernel then runs
    /// without its memory model. Use [`Kernel::try_new`] to get the error instead.
    pub fn new(mut config: Config, mem: Arc<dyn GuestMemory>) -> Arc<Kernel> {
        let vm = match config.memory.take().map(Vm::new).transpose() {
            Ok(vm) => vm,
            Err(e) => {
                (config.log)(&format!(
                    "the memory model is off, the configuration is wrong: {e}"
                ));
                None
            }
        };
        Self::build(config, mem, vm)
    }

    /// A kernel over a guest memory. The error names the value of `config.memory` that does not fit.
    pub fn try_new(mut config: Config, mem: Arc<dyn GuestMemory>) -> Result<Arc<Kernel>, String> {
        let vm = config.memory.take().map(Vm::new).transpose()?;
        Ok(Self::build(config, mem, vm))
    }

    fn build(config: Config, mem: Arc<dyn GuestMemory>, vm: Option<Vm>) -> Arc<Kernel> {
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
            vm,
            resource_limit: OnceLock::new(),
            system_events: SystemEvents::default(),
            tags: Mutex::new(std::collections::HashMap::new()),
        })
    }

    /// The memory model, or the error of a kernel that has none.
    pub(crate) fn vm(&self) -> KResultOr<&Vm> {
        self.vm.as_ref().ok_or(KResult::NOT_SUPPORTED)
    }

    /// Whether the kernel models the guest's memory.
    pub fn has_memory(&self) -> bool {
        self.vm.is_some()
    }

    /// The reservation as `(base, size)`, or none for a kernel without a memory model. The host names it to
    /// `nxkit_abi::guest::set_range`.
    pub fn guest_range(&self) -> Option<(u64, u64)> {
        self.vm.as_ref().map(|vm| (vm.cfg.base, vm.cfg.size))
    }

    fn check_priority(p: i32) -> KResultOr<()> {
        if (0..=MAX_PRIORITY).contains(&p) {
            Ok(())
        } else {
            Err(KResult::INVALID_PRIORITY)
        }
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

    /// Lends the QoS class of `cur` to the owner of a lock while `cur` waits, when the owner has a lower class. The
    /// Horizon kernel lends the waiter's priority the same way. The engine does it in `dm/host/hle_thread.c`, case
    /// 0x1A (ArbitrateLock), with `pthread_override_qos_class_start_np`. The returned guard ends the override. An
    /// owner that exits meanwhile is safe, see `lend.rs`.
    pub(crate) fn boost_owner(&self, cur: &Arc<Thread>, owner: u32) -> Option<LendGuard> {
        if !self.config.apply_qos {
            return None;
        }
        let waiter = cur.applied_qos()?;
        let owner = self.lock_owner(owner)?;
        let lower = owner.applied_qos().is_none_or(|q| q.raw() < waiter.raw());
        if !lower || owner.state() != ThreadState::Started {
            return None;
        }
        LendGuard::start(owner, waiter)
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

    /// The thread a lock word names: a kernel handle, or a tag the thread gave when it waited.
    pub(crate) fn lock_owner(&self, owner: u32) -> Option<Arc<Thread>> {
        if crate::handle::is_handle(owner) {
            self.thread_by_handle(owner).ok()
        } else {
            lock(&self.tags).get(&owner).and_then(Weak::upgrade)
        }
    }

    /// Notes that `t` uses `tag` in lock words, when the tag is not a kernel handle.
    pub(crate) fn learn_tag(&self, t: &Arc<Thread>, tag: u32) {
        if tag != 0 && !crate::handle::is_handle(tag) {
            lock(&self.tags).insert(tag, Arc::downgrade(t));
        }
    }

    fn resolve_thread(&self, cur: &Arc<Thread>, handle: u32) -> KResultOr<Arc<Thread>> {
        if handle == CURRENT_THREAD {
            Ok(cur.clone())
        } else {
            self.thread_by_handle(handle)
        }
    }

    /// `CloseHandle`. A session gets its `close`, transfer memory is unlocked and a SystemEvent binding of the
    /// handle is dropped.
    pub fn close_handle(&self, handle: u32) -> KResultOr<()> {
        let object = self.handles.remove(handle)?;
        self.system_events.unbind(handle);
        self.on_close(object);
        if let Some(hook) = self.config.on_close {
            hook(handle);
        }
        Ok(())
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

/// Marks the thread as exited when its body returns or panics.
struct ExitGuard(Arc<Kernel>, Arc<Thread>);

impl Drop for ExitGuard {
    fn drop(&mut self) {
        self.0.exit_thread(&self.1);
    }
}

#[cfg(test)]
mod tests {
    use super::stack_size;

    #[test]
    fn stack_sizes_are_defaulted_rounded_and_bounded_below() {
        assert_eq!(stack_size(0), 1 << 20);
        assert_eq!(stack_size(1), 64 << 10);
        assert_eq!(stack_size((64 << 10) + 1), 80 << 10);
        assert_eq!(stack_size(2 << 20), 2 << 20);
    }
}
