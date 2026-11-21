//! The executor of plain loads and stores: the register file, the address and the access.

use super::*;

/// The registers of a thread, as raw pointers into its saved state.
#[derive(Clone, Copy)]
pub(crate) struct Regs {
    /// `x0` to `x30` (31 registers in a row).
    pub(crate) x: *mut u64,
    /// The stack pointer.
    pub(crate) sp: *mut u64,
    /// `v0` to `v31`.
    pub(crate) v: *mut u128,
}

impl Regs {
    /// The value of general register `n`; 31 reads as zero.
    ///
    /// # Safety
    /// `self.x` must point at 31 readable registers.
    pub(super) unsafe fn gpr(&self, n: u8) -> u64 {
        if n >= 31 {
            0
        } else {
            // SAFETY: `n < 31` and the caller guarantees the 31 registers.
            unsafe { self.x.add(usize::from(n)).read() }
        }
    }

    /// The value of the base register `n`; 31 is the stack pointer.
    ///
    /// # Safety
    /// As for [`Regs::gpr`], and `self.sp` must be readable.
    pub(super) unsafe fn base(&self, n: u8) -> u64 {
        if n >= 31 {
            // SAFETY: the caller guarantees `sp`.
            unsafe { self.sp.read() }
        } else {
            // SAFETY: as for `gpr`.
            unsafe { self.gpr(n) }
        }
    }
}

/// The address an access uses and what it writes back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Plan {
    /// The first byte.
    pub(crate) ea: u64,
    /// The bytes in all (both registers of a pair).
    pub(crate) len: u64,
    /// The new value of the base register, when the mode writes back.
    pub(crate) writeback: Option<u64>,
}

/// The address of `op` with the registers `r`.
///
/// # Safety
/// `r` must point at a valid register file (see [`Regs`]).
pub(crate) unsafe fn plan(op: &Mem, r: &Regs) -> Plan {
    // SAFETY: the caller guarantees the register file.
    let base = unsafe { r.base(op.rn) };
    let (ea, writeback) = match op.mode {
        Mode::Offset(o) => (base.wrapping_add(o as u64), None),
        Mode::Pre(o) => {
            let a = base.wrapping_add(o as u64);
            (a, Some(a))
        }
        Mode::Post(o) => (base, Some(base.wrapping_add(o as u64))),
        Mode::Reg { rm, option, shift } => {
            // SAFETY: as above.
            let m = unsafe { r.gpr(rm) };
            let ext = match option {
                2 => u64::from(m as u32),
                6 => i64::from(m as u32 as i32) as u64,
                _ => m,
            };
            (base.wrapping_add(ext << shift), None)
        }
    };
    Plan {
        ea,
        len: u64::from(op.size) * if op.pair { 2 } else { 1 },
        writeback,
    }
}

/// Reads `n` bytes (1, 2, 4, 8 or 16) at `p`.
///
/// # Safety
/// `p` must be readable for `n` bytes.
pub(super) unsafe fn read(p: *const u8, n: usize, order: Ordering) -> u128 {
    let aligned = (p as usize).is_multiple_of(n);
    // SAFETY: readable per the caller; the typed pointers are aligned when `aligned`.
    unsafe {
        match n {
            1 => u128::from(AtomicU8::from_ptr(p.cast_mut()).load(order)),
            2 if aligned => u128::from(AtomicU16::from_ptr(p.cast_mut().cast()).load(order)),
            4 if aligned => u128::from(AtomicU32::from_ptr(p.cast_mut().cast()).load(order)),
            8 if aligned => u128::from(AtomicU64::from_ptr(p.cast_mut().cast()).load(order)),
            16 if (p as usize).is_multiple_of(8) => {
                let lo = AtomicU64::from_ptr(p.cast_mut().cast()).load(order);
                let hi = AtomicU64::from_ptr(p.cast_mut().add(8).cast()).load(order);
                u128::from(lo) | u128::from(hi) << 64
            }
            _ => {
                let mut b = [0u8; 16];
                for (k, d) in b.iter_mut().take(n).enumerate() {
                    *d = p.add(k).read_volatile();
                }
                u128::from_le_bytes(b)
            }
        }
    }
}

/// Writes the low `n` bytes of `v` at `p`.
///
/// # Safety
/// `p` must be writable for `n` bytes.
unsafe fn write(p: *mut u8, n: usize, v: u128, order: Ordering) {
    let aligned = (p as usize).is_multiple_of(n);
    // SAFETY: writable per the caller; the typed pointers are aligned when `aligned`.
    unsafe {
        match n {
            1 => AtomicU8::from_ptr(p).store(v as u8, order),
            2 if aligned => AtomicU16::from_ptr(p.cast()).store(v as u16, order),
            4 if aligned => AtomicU32::from_ptr(p.cast()).store(v as u32, order),
            8 if aligned => AtomicU64::from_ptr(p.cast()).store(v as u64, order),
            16 if (p as usize).is_multiple_of(8) => {
                AtomicU64::from_ptr(p.cast()).store(v as u64, order);
                AtomicU64::from_ptr(p.add(8).cast()).store((v >> 64) as u64, order);
            }
            _ => {
                let b = v.to_le_bytes();
                for (k, s) in b.iter().take(n).enumerate() {
                    p.add(k).write_volatile(*s);
                }
            }
        }
    }
}

/// Does the access: `p` is the byte of `plan.ea` in the mapping to use. Writes the loaded registers or the stored
/// bytes, then the base register for the modes that write back. Does not touch the program counter.
///
/// # Safety
/// `r` must be a valid register file, and `p` must be readable and writable for `plan.len` bytes.
pub(crate) unsafe fn perform(op: &Mem, r: &Regs, plan: &Plan, p: *mut u8) {
    let n = usize::from(op.size);
    let (load_order, store_order) = match op.order {
        Order::Acquire => (Ordering::Acquire, Ordering::Relaxed),
        Order::Release => (Ordering::Relaxed, Ordering::Release),
        Order::Plain => (Ordering::Relaxed, Ordering::Relaxed),
    };
    let regs = if op.pair { 2 } else { 1 };
    // Stores read both registers before anything is written.
    for k in 0..regs {
        let reg = if k == 0 { op.rt } else { op.rt2 };
        // SAFETY: `p` covers `len` bytes; the register file is valid per the caller.
        unsafe {
            let at = p.add(k * n);
            if op.load {
                let mut val = read(at, n, load_order);
                if op.signed {
                    let bits = (n * 8) as u32;
                    let s = 64 - bits;
                    let ext = (((val as u64) << s) as i64) >> s;
                    val = u128::from(if op.dest64 {
                        ext as u64
                    } else {
                        u64::from(ext as u32)
                    });
                }
                if op.simd {
                    r.v.add(usize::from(reg)).write_unaligned(val);
                } else if reg < 31 {
                    r.x.add(usize::from(reg)).write(val as u64);
                }
            } else {
                let val = if op.simd {
                    r.v.add(usize::from(reg)).read_unaligned()
                } else {
                    u128::from(r.gpr(reg))
                };
                write(at, n, val, store_order);
            }
        }
    }
    if let Some(w) = plan.writeback {
        // SAFETY: the register file is valid per the caller.
        unsafe {
            if op.rn >= 31 {
                r.sp.write(w);
            } else {
                r.x.add(usize::from(op.rn)).write(w);
            }
        }
    }
}
