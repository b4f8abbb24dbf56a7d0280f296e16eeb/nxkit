//! The `kernel` service: the Horizon thread model of `nxkit-hos` behind a C ABI.
//!
//! The host calls [`nxkit_kernel_init`] once, then [`nxkit_svc`] from the gate of each svc. A guest pointer is a
//! host pointer, so the guest memory of the kernel reads and writes the addresses it gets from the svc registers.
//! The gate answers a svc only when the `kernel` service is enabled in [`crate::nxkit_init`].

use nxkit_hos::{
    Config, GuestLauncher, Kernel, NameMatch, RoleRule, RoleTable, SvcRegs, Thread, ThreadRole,
    current_thread, sys,
};
use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};

/// The process-wide kernel. Set by the first successful [`nxkit_kernel_init`].
static KERNEL: OnceLock<Arc<Kernel>> = OnceLock::new();

/// Runs the guest code of a thread on the calling host thread and returns when the guest thread ends.
pub type NxkitThreadEntry = extern "C" fn(entry: u64, arg: u64, stack_top: u64, user: *mut c_void);

/// One rule of the role table. The first rule that matches a thread decides its role.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NxkitRoleRule {
    /// The thread name to match. NULL matches any name.
    pub name: *const c_char,
    /// How `name` matches: 1 the whole name, 2 the start, 3 anywhere. Any other value matches any name.
    pub name_match: u32,
    /// The lowest Switch priority. The priority is not checked when `prio_min` is greater than `prio_max`.
    pub prio_min: i32,
    /// The highest Switch priority.
    pub prio_max: i32,
    /// The guest address of the entry function, or 0 for any.
    pub entry: u64,
    /// The role: 0 render, 1 audio, 2 main, 3 loading, 4 streaming, 5 worker. Other values are refused.
    pub role: u32,
}

/// What [`nxkit_kernel_init`] takes.
#[repr(C)]
pub struct NxkitKernelConfig {
    /// Runs the guest code of a thread. Without it `CreateThread` is not answered.
    pub thread_entry: Option<NxkitThreadEntry>,
    /// Passed to `thread_entry`.
    pub user: *mut c_void,
    /// Non-zero turns the core rules of Horizon on.
    pub core_rules: u32,
    /// The role rules of the title, or NULL.
    pub roles: *const NxkitRoleRule,
    /// The number of rules.
    pub role_count: usize,
}

struct CLauncher {
    entry: NxkitThreadEntry,
    user: usize,
}

impl GuestLauncher for CLauncher {
    fn body(&self, entry: u64, arg: u64, stack_top: u64) -> Box<dyn FnOnce() + Send> {
        let (f, user) = (self.entry, self.user);
        Box::new(move || f(entry, arg, stack_top, user as *mut c_void))
    }
}

/// Reads the rules. A rule name is kept for the life of the process, as a rule needs a `&'static str`.
///
/// # Safety
///
/// `roles` points to `count` rules, and each non-NULL name is a NUL-terminated string.
unsafe fn read_rules(roles: *const NxkitRoleRule, count: usize) -> Option<Vec<RoleRule>> {
    if roles.is_null() || count == 0 {
        return Some(Vec::new());
    }
    // SAFETY: the caller promises `count` rules at `roles`.
    let raw = unsafe { std::slice::from_raw_parts(roles, count) };
    let mut rules = Vec::with_capacity(count);
    for r in raw {
        let role = *ThreadRole::ALL.get(r.role as usize)?;
        if role == ThreadRole::Unknown {
            return None;
        }
        let text: &'static str = if r.name.is_null() {
            ""
        } else {
            // SAFETY: the caller promises a NUL-terminated name.
            let s = unsafe { CStr::from_ptr(r.name) }
                .to_string_lossy()
                .into_owned();
            Box::leak(s.into_boxed_str())
        };
        let name = match (r.name.is_null(), r.name_match) {
            (false, 1) => NameMatch::Exact(text),
            (false, 2) => NameMatch::Prefix(text),
            (false, 3) => NameMatch::Contains(text),
            _ => NameMatch::Any,
        };
        let mut rule = RoleRule::named(name, role);
        if r.prio_min <= r.prio_max {
            rule = rule.with_priority(r.prio_min, r.prio_max);
        }
        if r.entry != 0 {
            rule = rule.with_entry(r.entry);
        }
        rules.push(rule);
    }
    Some(rules)
}

/// Creates the process-wide kernel. Returns 0 on success, 1 when a kernel exists already (the call changes
/// nothing) and -1 for a bad configuration.
///
/// # Safety
///
/// `config` points to a valid [`NxkitKernelConfig`]. Its rules and their names are valid for the call. Every guest
/// address that a later [`nxkit_svc`] receives is mapped, readable and writable while the process runs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nxkit_kernel_init(config: *const NxkitKernelConfig) -> i32 {
    if config.is_null() {
        return -1;
    }
    // SAFETY: the caller promises a valid configuration.
    let c = unsafe { &*config };
    // SAFETY: the caller promises valid rules.
    let Some(rules) = (unsafe { read_rules(c.roles, c.role_count) }) else {
        return -1;
    };
    let mut cfg = Config {
        roles: RoleTable::new(&rules),
        core_rules: c.core_rules != 0,
        ..Config::default()
    };
    if let Some(entry) = c.thread_entry {
        cfg.launcher = Some(Arc::new(CLauncher {
            entry,
            user: c.user as usize,
        }));
    }
    let mut created = false;
    KERNEL.get_or_init(|| {
        created = true;
        // SAFETY: guest addresses are host addresses, as the safety contract of this function says.
        let mem = unsafe { sys::HostMemory::new() };
        Kernel::new(cfg, Arc::new(mem))
    });
    if created { 0 } else { 1 }
}

/// The calling thread as a guest thread. A host thread the kernel does not know yet, such as the main thread, is
/// attached on its first svc.
fn caller(kernel: &Kernel) -> Option<Arc<Thread>> {
    current_thread().or_else(|| kernel.attach_current_thread("", 44, -2).ok())
}

/// Runs svc `number` with the registers `regs` (x0 to x7, read and written). Returns 1 when nxkit answered the call
/// and 0 when the host goes on with its own handler. Returns 0 before [`nxkit_kernel_init`] and when the `kernel`
/// service is not enabled. After `ExitThread` (0x0A) it returns 1 and the host ends the host thread.
///
/// # Safety
///
/// `regs` points to eight writable `uint64_t`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nxkit_svc(number: u32, regs: *mut u64) -> i32 {
    if regs.is_null() || crate::SERVICES.load(Ordering::Acquire) & crate::Service::Kernel.bit() == 0
    {
        return 0;
    }
    let Some(kernel) = KERNEL.get() else {
        return 0;
    };
    let Some(cur) = caller(kernel) else {
        return 0;
    };
    // SAFETY: the caller promises eight writable registers.
    let regs = unsafe { &mut *regs.cast::<SvcRegs>() };
    i32::from(kernel.dispatch_svc(&cur, number, regs))
}

/// Gives the thread `handle` the name the game set, so its role is chosen again. Returns the result code.
///
/// # Safety
///
/// `name` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nxkit_kernel_set_thread_name(handle: u32, name: *const c_char) -> u32 {
    let (Some(kernel), false) = (KERNEL.get(), name.is_null()) else {
        return nxkit_hos::KResult::INVALID_STATE.raw();
    };
    // SAFETY: the caller promises a NUL-terminated string.
    let name = unsafe { CStr::from_ptr(name) }.to_string_lossy();
    kernel
        .set_thread_name(handle, &name)
        .err()
        .map_or(0, |e| e.raw())
}

/// Writes the per-thread counters as text into `buf` of `capacity` bytes, with a NUL at the end, and returns the
/// length the whole text needs (without the NUL). The text is cut when it does not fit. Returns 0 before init.
///
/// # Safety
///
/// `buf` is NULL or points to `capacity` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nxkit_kernel_dump_stats(buf: *mut c_char, capacity: usize) -> usize {
    let Some(kernel) = KERNEL.get() else {
        return 0;
    };
    let text = kernel.dump_stats();
    if !buf.is_null() && capacity > 0 {
        let n = text.len().min(capacity - 1);
        // SAFETY: `buf` has `capacity` bytes and `n + 1 <= capacity`. The text does not overlap the buffer.
        unsafe {
            std::ptr::copy_nonoverlapping(text.as_ptr(), buf.cast::<u8>(), n);
            *buf.add(n) = 0;
        }
    }
    text.len()
}
