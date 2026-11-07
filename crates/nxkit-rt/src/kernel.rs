//! The `kernel` service: the Horizon thread model of `nxkit-hos` behind a C ABI.
//!
//! The host calls [`nxkit_kernel_init`] once, then [`nxkit_svc`] from the gate of each svc. A guest pointer is a
//! host pointer, so the guest memory of the kernel reads and writes the addresses it gets from the svc registers.
//! The gate answers a svc only when the `kernel` service is enabled in [`crate::nxkit_init`].

use nxkit_hos::{
    Config, GuestLauncher, Kernel, MAX_PRIORITY, MainThread, MemoryConfig, Module, NameMatch,
    RoleRule, RoleTable, SvcRegs, Thread, ThreadRole, current_thread, sys,
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
    /// The priority of the main thread from the NPDM (`main_thread.priority` in `game.json`), 0 to 63. The value
    /// -1 means the host gives none. The main thread then cannot attach and nxkit answers no svc of it.
    pub main_thread_priority: i32,
    /// The core of the main thread from the NPDM, 0 to 3, or -2 for the process default.
    pub main_thread_core: i32,
    /// The stack size of the main thread from the NPDM in bytes.
    pub main_thread_stack_size: u64,
    /// The process id that `GetProcessId` returns, or 0 for the default 0x80.
    pub process_id: u64,
    /// Ends the process for `ExitProcess`, `Break` and `ReturnFromException`, or NULL to call `exit(code)`.
    pub process_exit: Option<extern "C" fn(code: u32)>,
    /// The memory model, or NULL when the kernel does not model memory (no memory svc, `GetInfo` or IPC svc).
    pub memory: *const NxkitMemoryConfig,
}

/// One module (an NSO) in the front region of the reservation.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NxkitModule {
    /// The address of the text segment.
    pub base: u64,
    /// The size of the text segment.
    pub text: u64,
    /// The size of the read-only data segment.
    pub rodata: u64,
    /// The size of the data segment.
    pub data: u64,
    /// The size of the bss segment.
    pub bss: u64,
}

/// The guest's memory, as [`nxkit_hos::MemoryConfig`] describes it. Addresses are host addresses (R18).
#[repr(C)]
pub struct NxkitMemoryConfig {
    /// The base address of the reservation.
    pub base: u64,
    /// The size of the reservation.
    pub size: u64,
    /// The size of the front region at the start of the reservation.
    pub front_size: u64,
    /// The size of the stack region.
    pub stack_size: u64,
    /// The size of the alias region, at the end of the reservation.
    pub alias_size: u64,
    /// The memory pool of the application in bytes.
    pub app_pool: u64,
    /// What the process uses besides its heap in bytes.
    pub used_memory: u64,
    /// The system resource size of the NPDM, or 0.
    pub system_resource: u64,
    /// The program id that `GetInfo` returns.
    pub program_id: u64,
    /// The address of the thread-local area, inside the front region, readable and writable.
    pub tls_addr: u64,
    /// The size of the thread-local area. The first 0x4000 bytes are the exception context.
    pub tls_size: u64,
    /// The address of the area that backs shared memory, inside the front region, readable and writable.
    pub shared_addr: u64,
    /// The size of the shared memory area.
    pub shared_size: u64,
    /// The address of the main thread's stack, inside the stack region, or 0 for none.
    pub main_stack_addr: u64,
    /// The size of the main thread's stack.
    pub main_stack_size: u64,
    /// The modules.
    pub modules: *const NxkitModule,
    /// The number of modules.
    pub module_count: usize,
    /// Non-zero makes pages mapped by `MapPhysicalMemory` count against the pool.
    pub map_limit: u32,
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
        on_close: Some(nxkit_svc_system::on_handle_closed),
        ..Config::default()
    };
    if let Some(entry) = c.thread_entry {
        cfg.launcher = Some(Arc::new(CLauncher {
            entry,
            user: c.user as usize,
        }));
    }
    match main_thread(c) {
        Ok(main) => cfg.main_thread = main,
        Err(why) => {
            eprintln!("nxkit-rt: {why}");
            return -1;
        }
    }
    if c.process_id != 0 {
        cfg.process_id = c.process_id;
    }
    if let Some(exit) = c.process_exit {
        // The hook is set once. A second kernel is refused below, so the first hook stays the one that runs.
        let _ = PROCESS_EXIT.set(exit);
        cfg.process_exit = |code| match PROCESS_EXIT.get() {
            Some(exit) => exit(code),
            None => std::process::exit(code as i32),
        };
    }
    // SAFETY: the caller promises a valid memory configuration, or NULL.
    match unsafe { read_memory(c.memory) } {
        Ok(memory) => cfg.memory = memory,
        Err(why) => {
            eprintln!("nxkit-rt: {why}");
            return -1;
        }
    }
    if KERNEL.get().is_some() {
        return 1;
    }
    // SAFETY: guest addresses are host addresses, as the safety contract of this function says.
    let mem = unsafe { sys::HostMemory::new() };
    let kernel = match Kernel::try_new(cfg, Arc::new(mem)) {
        Ok(kernel) => kernel,
        Err(why) => {
            eprintln!("nxkit-rt: the kernel is not created: {why}");
            return -1;
        }
    };
    if KERNEL.set(kernel.clone()).is_err() {
        return 1;
    }
    if let Some(range) = kernel.guest_range() {
        nxkit_abi::guest::set_range(range.0, range.1);
    }
    if !nxkit_abi::kernel::set_kernel(kernel) {
        eprintln!(
            "nxkit-rt: a kernel is set in nxkit-abi already, the new kernel is not used by the services"
        );
    }
    0
}

/// The kernel that [`nxkit_kernel_init`] made, or `None` before it ran. The host Rust code uses it for the seams of the
/// services.
pub fn kernel() -> Option<&'static Arc<Kernel>> {
    KERNEL.get()
}

/// Attaches the calling host thread as the main guest thread, with the priority, core and stack size of the NPDM that
/// [`nxkit_kernel_init`] got. The host calls it on the thread that runs the game, before guest code. Gives the
/// reason when the kernel is missing or has no main thread.
pub fn attach_main_thread() -> Result<(), String> {
    let kernel = KERNEL
        .get()
        .ok_or_else(|| "the kernel is not started".to_owned())?;
    kernel
        .attach_main_thread("")
        .map(|_| ())
        .map_err(|e| format!("the main thread did not attach: {e:?}"))
}

/// The process exit hook of the host.
static PROCESS_EXIT: OnceLock<extern "C" fn(u32)> = OnceLock::new();

/// The main thread from the configuration. `-1` as priority means the host gave none.
fn main_thread(c: &NxkitKernelConfig) -> Result<Option<MainThread>, String> {
    if c.main_thread_priority == -1 {
        return Ok(None);
    }
    if !(0..=MAX_PRIORITY).contains(&c.main_thread_priority) {
        return Err(format!(
            "main_thread_priority {} is not -1 or 0 to {MAX_PRIORITY}",
            c.main_thread_priority
        ));
    }
    if !(0..=3).contains(&c.main_thread_core) && c.main_thread_core != -2 {
        return Err(format!(
            "main_thread_core {} is not 0 to 3 or -2",
            c.main_thread_core
        ));
    }
    Ok(Some(MainThread {
        priority: c.main_thread_priority,
        core: c.main_thread_core,
        stack_size: c.main_thread_stack_size,
    }))
}

/// Reads the memory configuration.
///
/// # Safety
///
/// `memory` is NULL or points to a valid [`NxkitMemoryConfig`] whose module list has `module_count` entries.
unsafe fn read_memory(memory: *const NxkitMemoryConfig) -> Result<Option<MemoryConfig>, String> {
    if memory.is_null() {
        return Ok(None);
    }
    // SAFETY: the caller promises a valid configuration.
    let m = unsafe { &*memory };
    let modules = if m.modules.is_null() || m.module_count == 0 {
        Vec::new()
    } else {
        // SAFETY: the caller promises `module_count` modules at `modules`.
        unsafe { std::slice::from_raw_parts(m.modules, m.module_count) }
            .iter()
            .map(|x| Module {
                base: x.base,
                text: x.text,
                rodata: x.rodata,
                data: x.data,
                bss: x.bss,
            })
            .collect()
    };
    let main_stack = (m.main_stack_size != 0).then_some((m.main_stack_addr, m.main_stack_size));
    // SAFETY: guest addresses are host addresses, as the safety contract of `nxkit_kernel_init` says.
    let host = unsafe { sys::HostMemory::new() };
    Ok(Some(MemoryConfig {
        host: Arc::new(host),
        base: m.base,
        size: m.size,
        front_size: m.front_size,
        stack_size: m.stack_size,
        alias_size: m.alias_size,
        app_pool: m.app_pool,
        used_memory: m.used_memory,
        system_resource: m.system_resource,
        map_limit: m.map_limit != 0,
        program_id: m.program_id,
        tls: (m.tls_addr, m.tls_size),
        shared: (m.shared_addr, m.shared_size),
        main_stack,
        modules,
    }))
}

/// The calling thread as a guest thread. A host thread the kernel does not know yet is the main thread. It is
/// attached on its first svc with the priority, core and stack size of the NPDM. Without them it does not attach.
fn caller(kernel: &Kernel) -> Option<Arc<Thread>> {
    current_thread().or_else(|| kernel.attach_main_thread("").ok())
}

/// Writes the addresses of the calling thread's thread-local region (`tpidrro_el0`) and TLS block (`tpidr_el0`)
/// to `tlr` and `tls`. The host's thread entry sets both registers from them before it runs guest code. Returns 0
/// on success and -1 when there is no kernel, the thread is not a guest thread or the kernel has no memory model.
///
/// # Safety
///
/// `tlr` and `tls` are NULL or point to a writable `uint64_t`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nxkit_kernel_thread_tls(tlr: *mut u64, tls: *mut u64) -> i32 {
    let Some(kernel) = KERNEL.get() else {
        return -1;
    };
    let Some(cur) = caller(kernel) else {
        return -1;
    };
    if cur.tlr() == 0 {
        return -1;
    }
    // SAFETY: the caller promises that a non-NULL pointer is writable.
    unsafe {
        if !tlr.is_null() {
            *tlr = cur.tlr();
        }
        if !tls.is_null() {
            *tls = cur.tls();
        }
    }
    0
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
