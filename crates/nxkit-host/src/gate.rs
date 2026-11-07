//! The gates of the rewritten code, in assembly, and the dispatcher behind them (R46).
//!
//! The rewriter replaces every `svc` and every counter read with a block that pushes `x29` and `x30`, loads the
//! address in the `KGATE` thread slot into `x30`, calls it and pops again. The address is a stub of this module,
//! one per thread. The stub puts the thread's gate stack in `x30` and enters `nxkit_gate`. The gate saves all
//! registers on that stack, never on the guest stack, because a Horizon svc leaves the guest stack as it was,
//! including the words below `sp`. It calls the dispatcher on the gate stack, loads the registers back and returns
//! to the block. It leaves `x18` alone.
//!
//! A thread leaves guest code in one step: `nxkit_thread_leave` loads the registers that `nxkit_thread_enter` saved
//! and returns to the caller of `enter`. The dispatcher runs on the gate stack, so no Rust frame of the host
//! thread is skipped and the frames it skips on the gate stack hold no value with a destructor.
//!
//! The assembly follows `dm/host/entry.S` of nxnative (see THIRD_PARTY.md).

use std::cell::Cell;
use std::sync::OnceLock;

use crate::gates::{GateTable, Kind};
use crate::stop;

/// The number of gate slots, one per guest thread that runs at the same time. It is the `.rept` count below.
pub const GATE_SLOTS: usize = 1024;
/// The size of a gate stack in bytes. It is the nesting check in the assembly.
pub const GATE_STACK_SIZE: u64 = 1 << 20;
/// The words of the register save area of `nxkit_thread_enter`.
const SAVE_WORDS: usize = 24;

const SVC_EXIT_THREAD: u32 = 0x0A;
const SVC_EXIT_PROCESS: u32 = 0x0B;
const SVC_GET_SYSTEM_TICK: u32 = 0x26;

std::arch::global_asm!(
    ".text",
    // The tops of the gate stacks, one per slot. Aligned to 16 KiB, so that stub k reaches its entry with `adrp` and an
    // immediate offset alone.
    ".globl _nxkit_gate_top",
    ".zerofill __DATA,__bss,_nxkit_gate_top,8192,14",
    // Stub k (16 bytes) serves gate slot k. x29 gets the return point, x30 the top of the slot's gate stack.
    ".globl _nxkit_gate_stubs",
    ".p2align 4",
    "_nxkit_gate_stubs:",
    ".set nxk_k, 0",
    ".rept 1024",
    "mov x29, x30",
    "adrp x30, _nxkit_gate_top@PAGE",
    "ldr x30, [x30, #(8 * nxk_k)]",
    "b _nxkit_gate",
    ".set nxk_k, nxk_k + 1",
    ".endr",
    // The register frame is 800 bytes at the top of the gate stack. x0..x30 at +0 (x30 is the return point), q0..q31
    // at +256, nzcv, fpsr and fpcr at +768, +776 and +784, and at +792 the sp of the gate block. The block holds the
    // guest's x29 and x30 at [sp] and [sp + 8].
    ".globl _nxkit_gate",
    ".p2align 2",
    "_nxkit_gate:",
    "sub x30, x30, #800",
    "stp x0, x1, [x30, #0]",
    "stp x2, x3, [x30, #16]",
    "stp x4, x5, [x30, #32]",
    "stp x6, x7, [x30, #48]",
    "stp x8, x9, [x30, #64]",
    "stp x10, x11, [x30, #80]",
    "stp x12, x13, [x30, #96]",
    "stp x14, x15, [x30, #112]",
    "stp x16, x17, [x30, #128]",
    "stp x18, x19, [x30, #144]",
    "stp x20, x21, [x30, #160]",
    "stp x22, x23, [x30, #176]",
    "stp x24, x25, [x30, #192]",
    "stp x26, x27, [x30, #208]",
    "str x28, [x30, #224]",
    "stp q0, q1, [x30, #256]",
    "stp q2, q3, [x30, #288]",
    "stp q4, q5, [x30, #320]",
    "stp q6, q7, [x30, #352]",
    "stp q8, q9, [x30, #384]",
    "stp q10, q11, [x30, #416]",
    "stp q12, q13, [x30, #448]",
    "stp q14, q15, [x30, #480]",
    "stp q16, q17, [x30, #512]",
    "stp q18, q19, [x30, #544]",
    "stp q20, q21, [x30, #576]",
    "stp q22, q23, [x30, #608]",
    "stp q24, q25, [x30, #640]",
    "stp q26, q27, [x30, #672]",
    "stp q28, q29, [x30, #704]",
    "stp q30, q31, [x30, #736]",
    "mrs x9, nzcv",
    "str x9, [x30, #768]",
    "mrs x9, fpsr",
    "str x9, [x30, #776]",
    "mrs x9, fpcr",
    "str x9, [x30, #784]",
    "ldr x9, [sp]",
    "stp x9, x29, [x30, #232]",
    "mov x9, sp",
    "str x9, [x30, #792]",
    // A gate that is entered on its own gate stack would overwrite the frame below it. No gate handler runs guest
    // code, so none is. Stop if one ever is.
    "add x10, x30, #800",
    "cmp x9, x10",
    "b.hs 1f",
    "sub x10, x10, #0x100000",
    "cmp x9, x10",
    "b.lo 1f",
    "mov sp, x30",
    "mov x0, x9",
    "bl _nxkit_gate_nested",
    "1:",
    "mov sp, x30",
    "ldr x29, [sp, #232]",
    "ldr x0, [sp, #240]",
    "mov x1, sp",
    "bl _nxkit_gate_dispatch",
    "ldr x9, [sp, #784]",
    "msr fpcr, x9",
    "ldr x9, [sp, #776]",
    "msr fpsr, x9",
    "ldr x9, [sp, #768]",
    "msr nzcv, x9",
    "ldp q0, q1, [sp, #256]",
    "ldp q2, q3, [sp, #288]",
    "ldp q4, q5, [sp, #320]",
    "ldp q6, q7, [sp, #352]",
    "ldp q8, q9, [sp, #384]",
    "ldp q10, q11, [sp, #416]",
    "ldp q12, q13, [sp, #448]",
    "ldp q14, q15, [sp, #480]",
    "ldp q16, q17, [sp, #512]",
    "ldp q18, q19, [sp, #544]",
    "ldp q20, q21, [sp, #576]",
    "ldp q22, q23, [sp, #608]",
    "ldp q24, q25, [sp, #640]",
    "ldp q26, q27, [sp, #672]",
    "ldp q28, q29, [sp, #704]",
    "ldp q30, q31, [sp, #736]",
    // The dispatcher may change any of x0..x28. The target of a counter read can be any of them. x18 is the platform
    // register, the rewritten guest code does not use it, and it is not loaded back.
    "ldp x0, x1, [sp, #0]",
    "ldp x2, x3, [sp, #16]",
    "ldp x4, x5, [sp, #32]",
    "ldp x6, x7, [sp, #48]",
    "ldp x8, x9, [sp, #64]",
    "ldp x10, x11, [sp, #80]",
    "ldp x12, x13, [sp, #96]",
    "ldp x14, x15, [sp, #112]",
    "ldp x16, x17, [sp, #128]",
    "ldr x19, [sp, #152]",
    "ldp x20, x21, [sp, #160]",
    "ldp x22, x23, [sp, #176]",
    "ldp x24, x25, [sp, #192]",
    "ldp x26, x27, [sp, #208]",
    "ldr x28, [sp, #224]",
    "ldr x30, [sp, #240]",
    "ldr x29, [sp, #792]",
    "mov sp, x29",
    "ret",
    // nxkit_thread_enter(entry, arg, sp, save, arg1): saves the callee-saved registers in `save`, switches to the guest
    // stack and calls `entry` with x0 = arg and x1 = arg1. A return from `entry` ends the thread like ExitThread.
    ".globl _nxkit_thread_enter",
    ".p2align 2",
    "_nxkit_thread_enter:",
    "stp x19, x20, [x3, #0]",
    "stp x21, x22, [x3, #16]",
    "stp x23, x24, [x3, #32]",
    "stp x25, x26, [x3, #48]",
    "stp x27, x28, [x3, #64]",
    "stp x29, x30, [x3, #80]",
    "stp d8, d9, [x3, #96]",
    "stp d10, d11, [x3, #112]",
    "stp d12, d13, [x3, #128]",
    "stp d14, d15, [x3, #144]",
    "mov x9, sp",
    "str x9, [x3, #160]",
    "mov sp, x2",
    "mov x29, #0",
    "mov x16, x0",
    "mov x0, x1",
    "mov x1, x4",
    "adr x30, 2f",
    "br x16",
    "2:",
    "bl _nxkit_thread_returned",
    "brk #0",
    // nxkit_thread_leave(save): the reverse of the first half of `nxkit_thread_enter`. It returns to the caller of
    // `nxkit_thread_enter`.
    ".globl _nxkit_thread_leave",
    ".p2align 2",
    "_nxkit_thread_leave:",
    "ldp x19, x20, [x0, #0]",
    "ldp x21, x22, [x0, #16]",
    "ldp x23, x24, [x0, #32]",
    "ldp x25, x26, [x0, #48]",
    "ldp x27, x28, [x0, #64]",
    "ldp x29, x30, [x0, #80]",
    "ldp d8, d9, [x0, #96]",
    "ldp d10, d11, [x0, #112]",
    "ldp d12, d13, [x0, #128]",
    "ldp d14, d15, [x0, #144]",
    "ldr x9, [x0, #160]",
    "mov sp, x9",
    "ret",
);

unsafe extern "C" {
    #[link_name = "nxkit_gate_top"]
    static mut GATE_TOP: [u64; GATE_SLOTS];
    #[link_name = "nxkit_gate_stubs"]
    fn gate_stubs();
    fn nxkit_thread_enter(entry: u64, arg: u64, sp: u64, save: *mut u64, arg1: u64);
    fn nxkit_thread_leave(save: *mut u64) -> !;
}

static TABLE: OnceLock<GateTable> = OnceLock::new();

thread_local! {
    /// The save area of the `enter` that runs on this thread, or 0 outside guest code.
    static SAVE: Cell<usize> = const { Cell::new(0) };
}

/// Installs the gates of all modules. It runs once, before the first guest thread starts.
pub fn install(table: GateTable) -> crate::error::Result<()> {
    TABLE
        .set(table)
        .map_err(|_| crate::error::HostError::Load("the gate table is installed twice".into()))
}

/// Sets the top of the gate stack of slot `k`. The thread that owns the slot calls it before it enters guest code.
pub fn set_slot_top(k: usize, top: u64) {
    if k >= GATE_SLOTS {
        return;
    }
    // SAFETY: `k` is inside the table. Only the thread that owns slot `k` writes the entry, and the stub of slot `k`
    // reads it on that thread only.
    unsafe { (&raw mut GATE_TOP).cast::<u64>().add(k).write(top & !15) };
}

/// The address of stub `k`, which a thread puts in its `KGATE` slot.
pub fn stub_address(k: usize) -> u64 {
    gate_stubs as usize as u64 + 16 * k as u64
}

/// Runs guest code on the calling host thread and returns when the guest thread ends, through ExitThread or by
/// returning from `entry`. `sp` is the guest stack. The thread slots must be set up. `entry` gets `arg` in `x0` and
/// `arg1` in `x1`.
pub fn enter(entry: u64, arg: u64, arg1: u64, sp: u64) {
    let mut save = [0u64; SAVE_WORDS];
    SAVE.with(|s| s.set(save.as_mut_ptr() as usize));
    // SAFETY: `entry` is guest code at a loaded address. `save` is live until the call returns, and the call returns
    // only through `nxkit_thread_leave` with this same pointer.
    unsafe { nxkit_thread_enter(entry, arg, sp, save.as_mut_ptr(), arg1) };
    SAVE.with(|s| s.set(0));
}

/// Runs svc `number` through the kernel with registers `regs`, at least eight. Returns what `nxkit_svc` returns:
/// 1 when it answered, 0 when it did not.
pub fn call_svc(number: u32, regs: &mut [u64]) -> i32 {
    if regs.len() < 8 {
        return 0;
    }
    // SAFETY: `regs` holds at least eight registers.
    unsafe { nxkit_rt::nxkit_svc(number, regs.as_mut_ptr()) }
}

/// The value of `tpidrro_el0` on this thread. On macOS it is the base of the thread-specific data.
pub fn tsd_base() -> u64 {
    let v: u64;
    // SAFETY: reading a thread register has no side effect.
    unsafe { std::arch::asm!("mrs {}, tpidrro_el0", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

/// Reads the word at byte offset `off` from the thread-specific data of this thread. It checks the layout that the
/// key page assumes.
pub fn tsd_word(off: u64) -> u64 {
    let base = tsd_base() & !7;
    // SAFETY: the thread-specific data of a thread is an array of at least 512 words, and the caller passes
    // `8 * key` of a key that `pthread_key_create` returned.
    unsafe { ((base + off) as usize as *const u64).read() }
}

fn leave() -> ! {
    let p = SAVE.with(Cell::get);
    if p == 0 {
        stop::fatal("a guest thread ended outside guest code");
    }
    // SAFETY: `p` is the save area of the live `enter` of this thread, see `enter`.
    unsafe { nxkit_thread_leave(p as *mut u64) }
}

#[unsafe(no_mangle)]
extern "C" fn nxkit_thread_returned() -> ! {
    leave()
}

#[unsafe(no_mangle)]
extern "C" fn nxkit_gate_nested(sp: u64) -> ! {
    stop::fatal(&format!("a gate was entered on its own gate stack, sp {sp:#x}: a gate handler ran guest code"))
}

fn svc(number: u32, regs: &mut [u64], ret: u64) {
    // SAFETY: `regs` holds 31 registers and `nxkit_svc` reads and writes the first eight.
    let answered = unsafe { nxkit_rt::nxkit_svc(number, regs.as_mut_ptr()) };
    match number {
        SVC_EXIT_THREAD if answered != 0 => leave(),
        SVC_EXIT_PROCESS => {
            stop::request("the game called ExitProcess");
            stop::park_forever()
        }
        _ if answered == 0 => stop::fatal(&format!(
            "svc {number:#x} at gate {ret:#x} is not answered: no kernel service handles it (R15)"
        )),
        _ => {}
    }
}

fn counter() -> u64 {
    let mut r = [0u64; 8];
    // SAFETY: `r` holds eight registers.
    let answered = unsafe { nxkit_rt::nxkit_svc(SVC_GET_SYSTEM_TICK, r.as_mut_ptr()) };
    if answered == 0 {
        stop::fatal("the counter read needs svc GetSystemTick (0x26) and the kernel does not answer it");
    }
    r[0]
}

/// Called by the assembly on the gate stack. `ret` is the address the gate block returns to, `regs` the frame.
#[unsafe(no_mangle)]
extern "C" fn nxkit_gate_dispatch(ret: u64, regs: *mut u64) {
    // SAFETY: the assembly passes its own frame, which holds x0..x30 as 31 words and is not shared.
    let regs = unsafe { std::slice::from_raw_parts_mut(regs, 31) };
    let Some(table) = TABLE.get() else {
        stop::fatal("a gate ran before the gate table was installed");
    };
    match table.find(ret) {
        Some(Kind::Svc(n)) => svc(n, regs, ret),
        Some(Kind::Counter(r)) => regs[usize::from(r)] = counter(),
        Some(Kind::Obs) => {}
        Some(Kind::Spin) => std::thread::yield_now(),
        None => stop::fatal(&format!("unknown gate at {ret:#x}")),
    }
}
