//! Big-stack entry for the exports that service crates ask for (R46).
//!
//! A game can give a thread a small stack. The save thread of one game has 8 KiB. A service function that opens a
//! file needs more, because it builds host paths. A service crate lists such functions in `big_stack_exports()`.
//! The loader puts a stub of this module into the import slot instead of the function.
//!
//! The stub loads the address of the function into `x16` and jumps to `nxkit_bigstack_enter`. The entry looks at the
//! stack pointer. Inside the reservation it is a guest stack, so the entry switches to a host stack of 512 KiB that
//! belongs to the thread, calls the function with `x0` to `x8` and `d0` to `d7` as they were, and switches back. Outside
//! the reservation, on a host thread or on the host stack already, it jumps straight to the function. Only register
//! arguments are carried over, so a function with more than eight integer or eight floating-point arguments, or a
//! variadic one, cannot be listed.
//!
//! The assembly follows `dm/host/hle_bigstack.c` of nxnative (see THIRD_PARTY.md).

use std::cell::Cell;
use std::sync::Mutex;

use crate::error::{HostError, Result};
use crate::sys;

/// The number of stubs. It is the `.rept` count below.
pub const STUBS: usize = 512;
/// The size of a host stack in bytes.
const STACK_SIZE: u64 = 512 << 10;
/// The guard at the low end of a host stack.
const GUARD: u64 = 0x4000;

std::arch::global_asm!(
    ".text",
    // The implementation addresses, one per stub. Aligned to 16 KiB, so that every stub reaches its word with `adrp`
    // and an immediate offset alone.
    ".globl _nxkit_bigstack_table",
    ".zerofill __DATA,__bss,_nxkit_bigstack_table,4096,14",
    // Stub k (16 bytes) loads word k of the table into x16 and enters. x16 and x17 are the intra-procedure scratch
    // registers, so the guest does not expect them to survive a call.
    ".globl _nxkit_bigstack_stubs",
    ".p2align 4",
    "_nxkit_bigstack_stubs:",
    ".set nxk_b, 0",
    ".rept 512",
    "adrp x16, _nxkit_bigstack_table@PAGE",
    "ldr x16, [x16, #(8 * nxk_b)]",
    "b _nxkit_bigstack_enter",
    "nop",
    ".set nxk_b, nxk_b + 1",
    ".endr",
    // x16 = the function. The frame on the original stack is 0xa0 bytes: +0x00 x0..x8 and x16, +0x50 d0..d7, +0x90
    // x29 and x30. After the switch the original sp and the return address of the guest are stored at the top of the
    // host stack, and the sp comes back when the function returns.
    ".globl _nxkit_bigstack_enter",
    ".p2align 2",
    "_nxkit_bigstack_enter:",
    "sub sp, sp, #0xa0",
    "stp x29, x30, [sp, #0x90]",
    "add x29, sp, #0x90",
    "stp x0, x1, [sp, #0x00]",
    "stp x2, x3, [sp, #0x10]",
    "stp x4, x5, [sp, #0x20]",
    "stp x6, x7, [sp, #0x30]",
    "stp x8, x16, [sp, #0x40]",
    "stp d0, d1, [sp, #0x50]",
    "stp d2, d3, [sp, #0x60]",
    "stp d4, d5, [sp, #0x70]",
    "stp d6, d7, [sp, #0x80]",
    "mov x0, sp",
    "bl _nxkit_bigstack_top",
    "mov x17, x0",
    "ldp x0, x1, [sp, #0x00]",
    "ldp x2, x3, [sp, #0x10]",
    "ldp x4, x5, [sp, #0x20]",
    "ldp x6, x7, [sp, #0x30]",
    "ldp x8, x16, [sp, #0x40]",
    "ldp d0, d1, [sp, #0x50]",
    "ldp d2, d3, [sp, #0x60]",
    "ldp d4, d5, [sp, #0x70]",
    "ldp d6, d7, [sp, #0x80]",
    "cbz x17, 1f",
    "mov x9, sp",
    "ldr x10, [x9, #0x98]",
    "mov sp, x17",
    "stp x9, x10, [sp, #-16]!",
    "blr x16",
    "ldr x9, [sp]",
    "mov sp, x9",
    "ldp x29, x30, [sp, #0x90]",
    "add sp, sp, #0xa0",
    "ret",
    "1:",
    "ldp x29, x30, [sp, #0x90]",
    "add sp, sp, #0xa0",
    "br x16",
);

unsafe extern "C" {
    #[link_name = "nxkit_bigstack_table"]
    static mut TABLE: [u64; STUBS];
    #[link_name = "nxkit_bigstack_stubs"]
    fn stubs();
}

/// The host stack of a thread. Dropped with the thread, it gives the stack back.
struct HostStack(Cell<u64>);

impl Drop for HostStack {
    fn drop(&mut self) {
        let top = self.0.get();
        if top != 0 {
            sys::free_range(top - STACK_SIZE, STACK_SIZE);
        }
    }
}

thread_local! {
    static STACK: HostStack = const { HostStack(Cell::new(0)) };
}

/// Called by the assembly with the stack pointer of the caller. It returns the top of the host stack of this thread,
/// or 0 when the caller is not on a guest stack and no switch is needed.
#[unsafe(no_mangle)]
extern "C" fn nxkit_bigstack_top(sp: u64) -> u64 {
    let (base, len) = nxkit_abi::guest::range();
    if sp < base || sp - base >= len {
        return 0;
    }
    STACK
        .try_with(|s| {
            if s.0.get() == 0 {
                match sys::map_stack(STACK_SIZE, GUARD) {
                    Ok(low) => s.0.set(low + STACK_SIZE),
                    Err(e) => {
                        eprintln!("nxkit-host: no big stack for this thread, staying on the guest stack ({e})");
                        return 0;
                    }
                }
            }
            s.0.get()
        })
        .unwrap_or(0)
}

/// The targets that have a stub, and the stub of each. The index is the stub number.
static WRAPPED: Mutex<Vec<(u64, u64)>> = Mutex::new(Vec::new());

/// The address of a stub that runs `target` on the big stack. The same target gets the same stub.
pub fn wrap(target: u64) -> Result<u64> {
    let mut list = WRAPPED
        .lock()
        .map_err(|_| HostError::Load("the big-stack table is poisoned".into()))?;
    if let Some((_, stub)) = list.iter().find(|(t, _)| *t == target) {
        return Ok(*stub);
    }
    let k = list.len();
    if k >= STUBS {
        return Err(HostError::Load(format!(
            "more than {STUBS} exports ask for the big stack, the host has no stub for {target:#x}"
        )));
    }
    // SAFETY: `k` is below the table length, the table is written here only and under the lock, and the stub that
    // reads word `k` is not reachable before the stub address is returned.
    unsafe { std::ptr::addr_of_mut!(TABLE).cast::<u64>().add(k).write_volatile(target) };
    let stub = stubs as usize as u64 + 16 * k as u64;
    list.push((target, stub));
    Ok(stub)
}
