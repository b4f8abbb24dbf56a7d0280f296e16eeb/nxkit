//! The pthread keys, the thread-local region of the kernel and the start of the kernel of `nxkit-rt`.

use super::*;

/// Creates a pthread key with no destructor.
pub fn key_create() -> Result<usize> {
    let mut k = 0usize;
    // SAFETY: `k` is a valid pointer.
    let r = unsafe { pthread_key_create(&mut k, None) };
    if r != 0 {
        return Err(HostError::Os {
            what: "pthread_key_create",
            errno: r,
        });
    }
    Ok(k)
}

/// Sets the value of a key for the current thread.
pub fn key_set(key: usize, value: u64) -> Result<()> {
    // SAFETY: the key came from `key_create`.
    let r = unsafe { pthread_setspecific(key, value as usize as *const c_void) };
    if r != 0 {
        return Err(HostError::Os {
            what: "pthread_setspecific",
            errno: r,
        });
    }
    Ok(())
}

/// Reads the value of a key for the current thread.
pub fn key_get(key: usize) -> u64 {
    // SAFETY: the key came from `key_create`.
    unsafe { pthread_getspecific(key) as usize as u64 }
}

/// The thread-local region and the ELF TLS block of the calling thread, as `(tlr, tls)`. The kernel makes them at the
/// first call of a thread and attaches the main thread then.
pub fn kernel_thread_tls() -> Result<(u64, u64)> {
    let (mut tlr, mut tls) = (0u64, 0u64);
    // SAFETY: both pointers are writable `u64` values on this stack.
    if unsafe { nxkit_rt::nxkit_kernel_thread_tls(&mut tlr, &mut tls) } != 0 {
        return Err(HostError::App(
            "the kernel gave no thread-local region to this thread".into(),
        ));
    }
    Ok((tlr, tls))
}

/// Starts the kernel of `nxkit-rt`. `thread_entry` runs every guest thread that the game creates. The kernel gets the
/// regions of the reservation, the memory profile and the main thread of the game. `core_rules` turns the core rules of
/// Horizon on, for a title that needs the same-core preemption (`core_preempt`).
pub fn kernel_init(
    thread_entry: nxkit_rt::NxkitThreadEntry,
    layout: &Layout,
    game: &Game,
) -> Result<()> {
    let game = &crate::kernel_map::configuration(game)?;
    let program_id =
        u64::from_str_radix(game.title_id.trim_start_matches("0x"), 16).map_err(|_| {
            HostError::Manifest(format!("title_id {:?} is not a hex number", game.title_id))
        })?;
    let modules: Vec<nxkit_rt::NxkitModule> = game
        .modules
        .iter()
        .map(|m| nxkit_rt::NxkitModule {
            base: m.base.saturating_add(m.text.offset),
            text: m.text.size,
            rodata: m.rodata.size,
            data: m.data.size,
            bss: m.bss_size,
        })
        .collect();
    let mem = &game.memory;
    let memory = nxkit_rt::NxkitMemoryConfig {
        base: layout.all.start,
        size: layout.all.len,
        front_size: layout.heap.start - layout.all.start,
        stack_size: layout.stack.len,
        alias_size: layout.alias.len,
        app_pool: mem.app_pool,
        used_memory: mem.mem_used,
        system_resource: mem.sys_resource,
        program_id,
        tls_addr: layout.tls.start,
        tls_size: layout.tls.len,
        shared_addr: layout.shared.start,
        shared_size: layout.shared.len,
        main_stack_addr: layout.main_stack.start,
        main_stack_size: layout.main_stack.len,
        modules: modules.as_ptr(),
        module_count: modules.len(),
        map_limit: u32::from(mem.map_limit),
    };
    let config = nxkit_rt::NxkitKernelConfig {
        thread_entry: Some(thread_entry),
        user: std::ptr::null_mut(),
        core_rules: u32::from(game.settings.core_preempt),
        roles: std::ptr::null(),
        role_count: 0,
        main_thread_priority: game.main_thread.priority,
        main_thread_core: game.main_thread.core,
        main_thread_stack_size: game.main_thread.stack_size,
        process_id: 0,
        process_exit: None,
        memory: &memory,
        core_mask: crate::npdm::core_mask(game).unwrap_or(0),
    };
    // SAFETY: `config`, `memory` and `modules` are valid for the call and there are no rules. The reservation is mapped
    // and registered with `nxkit_abi::guest` before any svc runs.
    match unsafe { nxkit_rt::nxkit_kernel_init(&config) } {
        0 => Ok(()),
        r => Err(HostError::App(format!("nxkit_kernel_init returned {r}"))),
    }
}
