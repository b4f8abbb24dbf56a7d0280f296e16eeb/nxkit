//! The report of a fatal signal: `SIGSEGV`, `SIGBUS`, `SIGILL`, `SIGFPE` and `SIGTRAP`.
//!
//! Without it a fault ends the host with status 139 and no word: the crash reporter of macOS writes no report for
//! most of them (the guest code is mapped by hand, outside dyld). The handler writes one block to stderr: the signal,
//! the address, the thread, the registers (in guest code they are the guest's own), and the frames of the frame
//! pointer chain, each as `module+offset` (a guest module, see `docs/diagnostics.md` and `sym.py`) or as the host image
//! and symbol that `dladdr` names. Then it puts back the handler it found (the stack overflow check of Rust's
//! runtime, or the default) and returns, so the fault happens again and ends the process as before, with the same
//! status. `NXKIT_CRASH_REPORT=0` installs nothing.
//!
//! The handler allocates nothing and formats into a buffer on its stack. It reads the frames through
//! `mach_vm_read_overwrite`, which fails instead of faulting on an unreadable address. `dladdr` takes the lock of
//! dyld; a fault inside dyld could hang there, which is the price of the names.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::fmt::Write;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// The signals the handler reports.
const SIGNALS: [(c_int, &str); 5] = [
    (SIGSEGV, "SIGSEGV"),
    (SIGBUS, "SIGBUS"),
    (SIGILL, "SIGILL"),
    (SIGFPE, "SIGFPE"),
    (SIGTRAP, "SIGTRAP"),
];
const SIGILL: c_int = 4;
const SIGTRAP: c_int = 5;
const SIGFPE: c_int = 8;
const SIGBUS: c_int = 10;
const SIGSEGV: c_int = 11;
const SA_ONSTACK: c_int = 0x1;
const SA_SIGINFO: c_int = 0x40;
/// The size of the alternate signal stack of a thread.
const ALT_STACK: usize = 64 << 10;
/// The most frames of the frame pointer chain.
const MAX_FRAMES: usize = 48;

/// `struct sigaction` of `<sys/signal.h>` (the user space one).
#[repr(C)]
#[derive(Clone, Copy)]
struct SigAction {
    handler: usize,
    mask: u32,
    flags: c_int,
}

/// `stack_t`.
#[repr(C)]
struct StackT {
    sp: *mut c_void,
    size: usize,
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

/// `ucontext_t` of arm64, up to `uc_mcontext`.
#[repr(C)]
struct UContext {
    onstack: c_int,
    sigmask: u32,
    stack: StackT,
    link: *mut c_void,
    mcsize: usize,
    mcontext: *const MContext,
}

/// `struct __darwin_mcontext64` of arm64: the exception state and the thread state.
#[repr(C)]
struct MContext {
    far: u64,
    esr: u32,
    exception: u32,
    x: [u64; 29],
    fp: u64,
    lr: u64,
    sp: u64,
    pc: u64,
    cpsr: u32,
    pad: u32,
}

/// `Dl_info`.
#[repr(C)]
struct DlInfo {
    fname: *const c_char,
    fbase: usize,
    sname: *const c_char,
    saddr: usize,
}

unsafe extern "C" {
    fn sigaction(sig: c_int, act: *const SigAction, old: *mut SigAction) -> c_int;
    fn sigaltstack(ss: *const StackT, old: *mut StackT) -> c_int;
    fn write(fd: c_int, buf: *const c_void, n: usize) -> isize;
    fn pthread_self() -> usize;
    fn pthread_getname_np(thread: usize, name: *mut c_char, len: usize) -> c_int;
    fn pthread_threadid_np(thread: usize, id: *mut u64) -> c_int;
    fn dladdr(addr: *const c_void, info: *mut DlInfo) -> c_int;
    static mach_task_self_: u32;
    fn mach_vm_read_overwrite(task: u32, addr: u64, size: u64, data: u64, out: *mut u64) -> c_int;
}

/// The handlers that were there before, by the index in [`SIGNALS`].
static OLD: OnceLock<[SigAction; SIGNALS.len()]> = OnceLock::new();
/// The guest modules: name, start, end.
static MODULES: OnceLock<Vec<(String, u64, u64)>> = OnceLock::new();
/// Set by the first report: a second fault (in the handler, or on another thread) only puts the old handler back.
static REPORTED: AtomicBool = AtomicBool::new(false);

/// Installs the handler for the process and an alternate signal stack for this thread, once. Nothing with
/// `NXKIT_CRASH_REPORT=0`.
pub fn install() {
    if std::env::var("NXKIT_CRASH_REPORT").is_ok_and(|v| v == "0") {
        return;
    }
    let mut olds = [SigAction {
        handler: 0,
        mask: 0,
        flags: 0,
    }; SIGNALS.len()];
    for (i, (sig, _)) in SIGNALS.iter().enumerate() {
        // SAFETY: a null action only reads the current one into `olds[i]`, which is a valid `struct sigaction`.
        unsafe { sigaction(*sig, std::ptr::null(), &mut olds[i]) };
    }
    if OLD.set(olds).is_err() {
        return;
    }
    thread_stack();
    let act = SigAction {
        handler: on_signal as extern "C" fn(c_int, *mut SigInfo, *mut c_void) as usize,
        mask: 0,
        flags: SA_SIGINFO | SA_ONSTACK,
    };
    for (sig, name) in SIGNALS {
        // SAFETY: `act` is a valid `struct sigaction` whose handler is an `extern "C"` function of the SA_SIGINFO shape
        // that lives for the whole process.
        if unsafe { sigaction(sig, &act, std::ptr::null_mut()) } != 0 {
            eprintln!("nxkit-host: no crash report for {name} (sigaction failed)");
        }
    }
}

/// Gives this thread an alternate signal stack when it has none, so a fault on an overflowed stack is still reported.
pub fn thread_stack() {
    if OLD.get().is_none() {
        return;
    }
    let mut cur = StackT {
        sp: std::ptr::null_mut(),
        size: 0,
        flags: 0,
    };
    // SAFETY: a null new stack only reads the current one into `cur`.
    if unsafe { sigaltstack(std::ptr::null(), &mut cur) } != 0 || !cur.sp.is_null() {
        return;
    }
    // The stack lives as long as the thread; a thread that ends leaves its 64 KiB behind.
    let mem: &'static mut [u8] = Box::leak(vec![0u8; ALT_STACK].into_boxed_slice());
    let ss = StackT {
        sp: mem.as_mut_ptr().cast(),
        size: mem.len(),
        flags: 0,
    };
    // SAFETY: `ss` names memory that is never freed.
    unsafe { sigaltstack(&ss, std::ptr::null_mut()) };
}

/// `module+offset` of a guest address, None outside the guest modules.
pub fn guest_place(addr: u64) -> Option<String> {
    MODULES
        .get()?
        .iter()
        .find(|(_, s, e)| (*s..*e).contains(&addr))
        .map(|(n, s, _)| format!("{n}+{:#x}", addr - s))
}

/// Names the guest modules for the report: name, start and end of each.
pub fn set_modules(modules: Vec<(String, u64, u64)>) {
    let _ = MODULES.set(modules);
}

/// A writer into a fixed buffer; what does not fit is cut.
struct Buf {
    b: [u8; 8192],
    n: usize,
}

impl Write for Buf {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        let k = s.len().min(self.b.len() - self.n);
        self.b[self.n..self.n + k].copy_from_slice(&s.as_bytes()[..k]);
        self.n += k;
        Ok(())
    }
}

impl Buf {
    fn flush(&mut self) {
        // SAFETY: `b[..n]` is initialised; fd 2 is stderr.
        unsafe { write(2, self.b.as_ptr().cast(), self.n) };
        self.n = 0;
    }
}

/// The `u64` at `addr`, None when it cannot be read.
fn read_u64(addr: u64) -> Option<u64> {
    if addr == 0 || !addr.is_multiple_of(8) {
        return None;
    }
    let mut v = 0u64;
    let mut out = 0u64;
    // SAFETY: the kernel copies 8 bytes into `v` or fails; it never faults on `addr`.
    let r =
        unsafe { mach_vm_read_overwrite(mach_task_self_, addr, 8, &raw mut v as u64, &mut out) };
    (r == 0 && out == 8).then_some(v)
}

/// Writes where `addr` is: a guest module and offset, or the host image and symbol.
fn place(w: &mut Buf, addr: u64) {
    if let Some((name, start, _)) = MODULES
        .get()
        .and_then(|m| m.iter().find(|(_, s, e)| (*s..*e).contains(&addr)))
    {
        let _ = write!(w, "{name}+{:#x}", addr - start);
        return;
    }
    let mut info = DlInfo {
        fname: std::ptr::null(),
        fbase: 0,
        sname: std::ptr::null(),
        saddr: 0,
    };
    // SAFETY: `info` is a valid `Dl_info`; `dladdr` only looks the address up.
    if addr == 0 || unsafe { dladdr(addr as *const c_void, &mut info) } == 0 || info.fname.is_null()
    {
        let _ = write!(w, "?");
        return;
    }
    // SAFETY: dyld gives NUL-terminated strings that live while the image is loaded.
    let file = unsafe { CStr::from_ptr(info.fname) }.to_bytes();
    let file = file.rsplit(|c| *c == b'/').next().unwrap_or(file);
    let _ = write!(
        w,
        "{}+{:#x}",
        std::str::from_utf8(file).unwrap_or("?"),
        addr - info.fbase as u64
    );
    if !info.sname.is_null() {
        // SAFETY: as above.
        let s = unsafe { CStr::from_ptr(info.sname) }.to_bytes();
        let _ = write!(
            w,
            " {}+{:#x}",
            std::str::from_utf8(s).unwrap_or("?"),
            addr - info.saddr as u64
        );
    }
}

extern "C" fn on_signal(sig: c_int, info: *mut SigInfo, uctx: *mut c_void) {
    let idx = SIGNALS.iter().position(|(s, _)| *s == sig);
    if !REPORTED.swap(true, Ordering::SeqCst) {
        report(sig, idx, info, uctx);
    }
    // Put back what was there and return: the fault happens again and that handler (or the default) ends it.
    if let (Some(i), Some(olds)) = (idx, OLD.get()) {
        // SAFETY: `olds[i]` is the action `sigaction` gave for this signal.
        unsafe { sigaction(sig, &olds[i], std::ptr::null_mut()) };
    }
}

fn report(sig: c_int, idx: Option<usize>, info: *mut SigInfo, uctx: *mut c_void) {
    let mut w = Buf { b: [0; 8192], n: 0 };
    let name = idx.map_or("signal", |i| SIGNALS[i].1);
    // SAFETY: the kernel passes a valid `siginfo_t` to an SA_SIGINFO handler.
    let (code, addr) = unsafe { info.as_ref() }.map_or((0, 0), |i| (i.code, i.addr));
    let mut tname = [0 as c_char; 64];
    let mut tid = 0u64;
    // SAFETY: the buffer holds 64 bytes and is NUL-terminated by the call (it was zeroed).
    unsafe {
        let me = pthread_self();
        pthread_getname_np(me, tname.as_mut_ptr(), tname.len() - 1);
        pthread_threadid_np(me, &mut tid);
    }
    // SAFETY: `tname` is NUL-terminated.
    let tname = unsafe { CStr::from_ptr(tname.as_ptr()) }.to_string_lossy();
    let _ = writeln!(
        w,
        "\nnxkit-host: fatal {name} ({sig}, code {code}) at address {addr:#x} on thread \"{tname}\" (tid {tid})"
    );
    // SAFETY: the kernel passes a valid `ucontext_t` whose `uc_mcontext` points at the saved state.
    let mc =
        unsafe { (uctx as *const UContext).as_ref() }.and_then(|u| unsafe { u.mcontext.as_ref() });
    let Some(mc) = mc else {
        w.flush();
        return;
    };
    let _ = write!(w, "  pc {:#x} ", mc.pc);
    place(&mut w, mc.pc);
    let _ = write!(w, "\n  lr {:#x} ", mc.lr);
    place(&mut w, mc.lr);
    let _ = writeln!(
        w,
        "\n  sp {:#x} fp {:#x} far {:#x} esr {:#x} cpsr {:#x}",
        mc.sp, mc.fp, mc.far, mc.esr, mc.cpsr
    );
    for (i, x) in mc.x.iter().enumerate() {
        let _ = write!(w, "  x{i:<2} {x:#018x}");
        if i % 4 == 3 {
            let _ = writeln!(w);
        }
    }
    let _ = writeln!(w);
    w.flush();
    let _ = writeln!(w, "  frames (frame pointer chain):");
    let mut fp = mc.fp;
    for n in 0..MAX_FRAMES {
        let (Some(next), Some(ret)) = (read_u64(fp), read_u64(fp.wrapping_add(8))) else {
            break;
        };
        if ret == 0 {
            break;
        }
        let _ = write!(w, "  #{n:<2} {ret:#x} ");
        place(&mut w, ret);
        let _ = writeln!(w);
        if next <= fp {
            break;
        }
        fp = next;
    }
    if let Some(m) = MODULES.get() {
        let _ = write!(w, "  guest modules:");
        for (name, s, e) in m {
            let _ = write!(w, " {name} {s:#x}..{e:#x}");
        }
        let _ = writeln!(w);
    }
    w.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_match_the_sdk() {
        assert_eq!(std::mem::size_of::<SigAction>(), 16);
        assert_eq!(std::mem::offset_of!(SigInfo, addr), 24);
        assert_eq!(std::mem::offset_of!(UContext, mcontext), 48);
        assert_eq!(std::mem::offset_of!(MContext, x), 16);
        assert_eq!(std::mem::offset_of!(MContext, fp), 248);
        assert_eq!(std::mem::offset_of!(MContext, pc), 272);
    }

    #[test]
    fn buffer_cuts_what_does_not_fit() {
        let mut w = Buf { b: [0; 8192], n: 0 };
        let long = "x".repeat(9000);
        let _ = w.write_str(&long);
        assert_eq!(w.n, 8192);
    }

    #[test]
    fn unreadable_words_are_none() {
        assert_eq!(read_u64(0), None);
        assert_eq!(read_u64(8), None);
        let v = 0x1234_5678_u64;
        assert_eq!(read_u64(&raw const v as u64), Some(v));
    }
}
