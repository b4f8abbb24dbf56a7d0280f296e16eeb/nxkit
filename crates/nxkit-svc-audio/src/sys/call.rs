//! Calls into the game's SDK code.

#![allow(unsafe_code)]

/// Calls the guest function at `addr` with up to eight integer arguments and returns `x0`.
///
/// `addr` must come from the host's symbol lookup (`host::Host::guest_sym`), so it is the entry of an SDK function that
/// takes only integer or pointer arguments. A zero address returns 0. Extra arguments are harmless on AArch64, because
/// they sit in registers the callee does not read.
pub fn call(addr: usize, a: [u64; 8]) -> u64 {
    if addr == 0 {
        return 0;
    }
    type F = extern "C" fn(u64, u64, u64, u64, u64, u64, u64, u64) -> u64;
    // SAFETY: the caller got `addr` from the host's guest symbol lookup, which names a function with the C calling
    // convention.
    let f: F = unsafe { std::mem::transmute::<usize, F>(addr) };
    f(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7])
}

/// Like [`call`] for a function that returns a 16 byte structure in `x0` and `x1`.
pub fn call_pair(addr: usize, a: [u64; 8]) -> (u64, u64) {
    if addr == 0 {
        return (0, 0);
    }
    #[repr(C)]
    struct Pair(u64, u64);
    type F = extern "C" fn(u64, u64, u64, u64, u64, u64, u64, u64) -> Pair;
    // SAFETY: as in `call`. A 16 byte structure of two integers is returned in `x0` and `x1`.
    let f: F = unsafe { std::mem::transmute::<usize, F>(addr) };
    let r = f(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7]);
    (r.0, r.1)
}

/// Reads 4 bytes of an AArch64 instruction at `addr`. Returns `None` for a zero or misaligned address.
pub fn read_insn(addr: usize) -> Option<u32> {
    if addr == 0 || !addr.is_multiple_of(4) {
        return None;
    }
    // SAFETY: `addr` is the entry of SDK code that the host mapped, which is readable and 4 byte aligned.
    Some(unsafe { std::ptr::read(addr as *const u32) })
}

/// Reads the 8 byte pointer at `addr` (a GOT slot). Returns `None` for a zero or misaligned address.
pub fn read_ptr(addr: usize) -> Option<usize> {
    if addr == 0 || !addr.is_multiple_of(8) {
        return None;
    }
    // SAFETY: `addr` is a GOT slot of the game's module, which is mapped, readable and 8 byte aligned.
    Some(unsafe { std::ptr::read(addr as *const usize) })
}
