//! The signal handler, the handler before it, the namer of program counters and the alias mapping.

use super::*;

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
            on_tracked(pg, w, pc, write, addr, uctx);
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

/// Serves a fault on the tracked page `pg`. Atomics, `mprotect` and nothing else (the emulation of an access adds the
/// access itself).
fn on_tracked(pg: usize, w: &AtomicU32, pc: u64, write: bool, addr: u64, uctx: *mut c_void) {
    let internal = is_internal();
    let origin = origin_of(pc, internal);
    let mirror = SHARED.mirror.load(Ordering::Acquire);
    if mirror != 0 {
        if !internal && try_emulate(pg, w, pc, addr, uctx) {
            return;
        }
        let kind = serve_mirrored(pg, w, write, internal, mirror, addr);
        SHARED.faults[kind as usize][origin as usize].fetch_add(1, Ordering::Relaxed);
        if !internal {
            count_pc(kind, pc);
        }
        let base = SHARED.base.load(Ordering::Relaxed);
        if write && matches!(kind, FaultKind::CleanWrite | FaultKind::WriteInFlight) {
            let (ph, newest) = phase_now();
            SHARED.phase_writes[ph].fetch_add(1, Ordering::Relaxed);
            if ph == 1 {
                let r = newest as usize % P1_RING;
                let n = SHARED.p1_n[r].fetch_add(1, Ordering::AcqRel) as usize;
                if n < P1_CAP {
                    SHARED.p1_log[r][n].store(addr.wrapping_sub(base) + 1, Ordering::Release);
                }
                if !internal {
                    count_pc_in(3, pc);
                }
            }
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

pub(super) fn install_handler() {
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

/// Makes a second mapping of `[base, base + len)` with read and write access that shares the pages of the first (the
/// pages are the same VM object, so a store through one is a store through the other, and the protection of the two
/// mappings is independent). Every VM region of the range is mapped by itself, so holes and regions without write
/// permission leave a gap in the alias instead of failing the whole call. Returns the address of the byte at `base`.
pub(super) fn build_alias(base: u64, len: u64) -> Result<u64, String> {
    // SAFETY: an anonymous reservation without access; the kernel picks the address.
    let target = unsafe {
        mmap(
            std::ptr::null_mut(),
            len as usize,
            PROT_NONE,
            MAP_PRIVATE | MAP_ANON | MAP_NORESERVE,
            -1,
            0,
        )
    };
    if target as isize == -1 {
        return Err("no address range for the alias".to_string());
    }
    let target = target as u64;
    let end = base + len;
    let (mut at, mut mapped) = (base, 0u64);
    while at < end {
        let Some(r) = crate::vmregion::region_at_or_after(at) else {
            break;
        };
        if r.start >= end {
            break;
        }
        let (s, e) = (r.start.max(at), (r.start + r.size).min(end));
        at = e;
        if e <= s {
            continue;
        }
        let before = (r.object, r.offset_of(s));
        let (mut t, mut cur, mut max) = (target + (s - base), 0, 0);
        // SAFETY: the source range lies in this task; the target range is inside the reservation made above, which the
        // call replaces with the second mapping (no copy: the pages are shared).
        let kr = unsafe {
            mach_vm_remap(
                mach_task_self_,
                &mut t,
                e - s,
                0,
                VM_FLAGS_FIXED | VM_FLAGS_OVERWRITE,
                mach_task_self_,
                s,
                0,
                &mut cur,
                &mut max,
                VM_INHERIT_NONE,
            )
        };
        if kr != 0 || t != target + (s - base) {
            continue;
        }
        // SAFETY: the range was mapped by the call above.
        let ok = unsafe { mach_vm_protect(mach_task_self_, t, e - s, 0, PROT_RW) } == 0;
        if !ok {
            continue;
        }
        // The first mapping must be the same object after the call (a copy would leave the GPU on the old pages). A region
        // that was never touched has no object yet (id 0): the call makes one, and no page can be under the GPU.
        let after = crate::vmregion::vm_region(s).map(|r| (r.object, r.offset_of(s)));
        if before.0 != 0 && after != Some(before) {
            // SAFETY: the reservation made above.
            unsafe { munmap(target as *mut c_void, len as usize) };
            return Err("the remap moved the guest pages to another VM object".to_string());
        }
        mapped += e - s;
    }
    if mapped == 0 {
        // SAFETY: the reservation made above.
        unsafe { munmap(target as *mut c_void, len as usize) };
        return Err("no region of guest memory could be mapped twice".to_string());
    }
    Ok(target)
}
