//! The executor of the atomics, the exclusives and `LD1`/`ST1`.

use super::*;

/// The address of `op`, with what it writes back.
///
/// # Safety
/// `r` must be a valid register file.
pub(crate) unsafe fn plan_op(op: &Op, r: &Regs) -> Plan {
    // SAFETY: the caller guarantees the register file.
    unsafe {
        match op {
            Op::Mem(m) => plan(m, r),
            Op::Rmw { size, rn, .. } => Plan {
                ea: r.base(*rn),
                len: u64::from(*size),
                writeback: None,
            },
            Op::Cas { size, pair, rn, .. } => Plan {
                ea: r.base(*rn),
                len: u64::from(*size) * if *pair { 2 } else { 1 },
                writeback: None,
            },
            Op::Excl { size, pair, rn, .. } => Plan {
                ea: r.base(*rn),
                len: u64::from(*size) * if *pair { 2 } else { 1 },
                writeback: None,
            },
            Op::Ld1 {
                q, regs, rn, post, ..
            } => {
                let len = u64::from(*regs) * if *q { 16 } else { 8 };
                let ea = r.base(*rn);
                let writeback = post.map(|m| ea.wrapping_add(if m == 31 { len } else { r.gpr(m) }));
                Plan { ea, len, writeback }
            }
            Op::Lane { size, rn, post, .. } => {
                let len = u64::from(*size);
                let ea = r.base(*rn);
                let writeback = post.map(|m| ea.wrapping_add(if m == 31 { len } else { r.gpr(m) }));
                Plan { ea, len, writeback }
            }
        }
    }
}

fn ords(acq: bool, rel: bool) -> Ordering {
    match (acq, rel) {
        (true, true) => Ordering::AcqRel,
        (true, false) => Ordering::Acquire,
        (false, true) => Ordering::Release,
        _ => Ordering::Relaxed,
    }
}

fn fail_ord(o: Ordering) -> Ordering {
    if matches!(o, Ordering::Acquire | Ordering::AcqRel) {
        Ordering::Acquire
    } else {
        Ordering::Relaxed
    }
}

/// Does `op` on the aligned memory at `p` and returns the old value.
///
/// # Safety
/// `p` must be valid for `size` bytes and aligned to them.
unsafe fn rmw(p: *mut u8, size: u8, op: RmwOp, v: u64, o: Ordering) -> u64 {
    macro_rules! go {
        ($a:ty, $s:ty, $u:ty, $i:ty) => {{
            // SAFETY: per the caller.
            let (a, sa) = unsafe { (<$a>::from_ptr(p.cast()), <$s>::from_ptr(p.cast())) };
            let v = v as $u;
            (match op {
                RmwOp::Add => a.fetch_add(v, o),
                RmwOp::Clr => a.fetch_and(!v, o),
                RmwOp::Eor => a.fetch_xor(v, o),
                RmwOp::Set => a.fetch_or(v, o),
                RmwOp::Umax => a.fetch_max(v, o),
                RmwOp::Umin => a.fetch_min(v, o),
                RmwOp::Swp => a.swap(v, o),
                RmwOp::Smax => sa.fetch_max(v as $i, o) as $u,
                RmwOp::Smin => sa.fetch_min(v as $i, o) as $u,
            }) as u64
        }};
    }
    use std::sync::atomic::{AtomicI8, AtomicI16, AtomicI32, AtomicI64};
    match size {
        1 => go!(AtomicU8, AtomicI8, u8, i8),
        2 => go!(AtomicU16, AtomicI16, u16, i16),
        4 => go!(AtomicU32, AtomicI32, u32, i32),
        _ => go!(AtomicU64, AtomicI64, u64, i64),
    }
}

/// Compare-exchange of `size` bytes (1 to 8) at the aligned `p`; returns the old value.
///
/// # Safety
/// `p` must be valid for `size` bytes and aligned to them.
unsafe fn cas(p: *mut u8, size: u8, old: u64, new: u64, o: Ordering) -> u64 {
    macro_rules! go {
        ($a:ty, $u:ty) => {{
            // SAFETY: per the caller.
            let a = unsafe { <$a>::from_ptr(p.cast()) };
            match a.compare_exchange(old as $u, new as $u, o, fail_ord(o)) {
                Ok(v) | Err(v) => v as u64,
            }
        }};
    }
    match size {
        1 => go!(AtomicU8, u8),
        2 => go!(AtomicU16, u16),
        4 => go!(AtomicU32, u32),
        _ => go!(AtomicU64, u64),
    }
}

/// A 16-byte compare-exchange (`CASPAL`); returns the old value as (low, high).
///
/// # Safety
/// `p` must be valid for 16 bytes and 16-byte aligned.
unsafe fn cas16(p: *mut u8, old: (u64, u64), new: (u64, u64)) -> (u64, u64) {
    let (mut o0, mut o1) = old;
    // SAFETY: per the caller; the registers are the even pairs the instruction wants.
    unsafe {
        std::arch::asm!(
            ".arch_extension lse",
            "caspal x2, x3, x4, x5, [{p}]",
            p = in(reg) p,
            inout("x2") o0,
            inout("x3") o1,
            in("x4") new.0,
            in("x5") new.1,
            options(nostack),
        );
    }
    (o0, o1)
}

/// What the last emulated load-exclusive of a thread read: address, bytes in all, value (low, high).
type Slot = (u64, u64, u64, u64);

thread_local! {
    /// The monitor of the thread: set by an emulated `LDXR`/`LDXP`, checked and cleared by an emulated `STXR`/`STXP`.
    static MONITOR: std::cell::Cell<Slot> = const { std::cell::Cell::new((0, 0, 0, 0)) };
}

/// Does `op` at `p` (the alias of `plan.ea`). Returns false, with nothing changed, when the access is not aligned the way
/// an atomic needs; the caller takes the ordinary path.
///
/// # Safety
/// `r` must be a valid register file, and `p` readable and writable for `plan.len` bytes.
pub(crate) unsafe fn perform_op(op: &Op, r: &Regs, plan: &Plan, p: *mut u8) -> bool {
    let set = |n: u8, v: u64| {
        if n < 31 {
            // SAFETY: the register file is valid per the caller.
            unsafe { r.x.add(usize::from(n)).write(v) };
        }
    };
    let mask = |size: u8, v: u64| {
        if size >= 8 {
            v
        } else {
            v & ((1u64 << (u32::from(size) * 8)) - 1)
        }
    };
    match op {
        Op::Mem(m) => {
            // SAFETY: per the caller.
            unsafe { perform(m, r, plan, p) };
            true
        }
        Op::Rmw {
            op,
            size,
            rs,
            rt,
            acq,
            rel,
            ..
        } => {
            if !(p as usize).is_multiple_of(usize::from(*size)) {
                return false;
            }
            // SAFETY: per the caller.
            unsafe {
                let v = mask(*size, r.gpr(*rs));
                let old = rmw(p, *size, *op, v, ords(*acq, *rel));
                set(*rt, old);
            }
            true
        }
        Op::Cas {
            size,
            pair,
            rs,
            rt,
            acq,
            rel,
            ..
        } => {
            let total = usize::from(*size) * if *pair { 2 } else { 1 };
            if !(p as usize).is_multiple_of(total) {
                return false;
            }
            let o = ords(*acq, *rel);
            // SAFETY: per the caller.
            unsafe {
                if !*pair {
                    let old = cas(
                        p,
                        *size,
                        mask(*size, r.gpr(*rs)),
                        mask(*size, r.gpr(*rt)),
                        o,
                    );
                    set(*rs, old);
                } else if *size == 4 {
                    let e = mask(4, r.gpr(*rs)) | mask(4, r.gpr(*rs + 1)) << 32;
                    let n = mask(4, r.gpr(*rt)) | mask(4, r.gpr(*rt + 1)) << 32;
                    let old = cas(p, 8, e, n, o);
                    set(*rs, old & 0xffff_ffff);
                    set(*rs + 1, old >> 32);
                } else {
                    let old = cas16(
                        p,
                        (r.gpr(*rs), r.gpr(*rs + 1)),
                        (r.gpr(*rt), r.gpr(*rt + 1)),
                    );
                    set(*rs, old.0);
                    set(*rs + 1, old.1);
                }
            }
            true
        }
        Op::Excl {
            load,
            pair,
            size,
            rs,
            rt,
            rt2,
            ord,
            ..
        } => {
            let total = u64::from(*size) * if *pair { 2 } else { 1 };
            if !(p as usize).is_multiple_of(total as usize) {
                return false;
            }
            let ea = plan.ea;
            if *load {
                // SAFETY: per the caller.
                let (lo, hi) = unsafe {
                    let o = if *ord {
                        Ordering::Acquire
                    } else {
                        Ordering::Relaxed
                    };
                    if *pair && *size == 8 {
                        // Two words: read them as one 16-byte unit (a compare-exchange with itself would write; two
                        // atomic reads are enough for the value the store compares against).
                        (read(p, 8, o) as u64, read(p.add(8), 8, o) as u64)
                    } else if *pair {
                        let v = read(p, 8, o) as u64;
                        (v & 0xffff_ffff, v >> 32)
                    } else {
                        (read(p, usize::from(*size), o) as u64, 0)
                    }
                };
                set(*rt, lo);
                if *pair {
                    set(*rt2, hi);
                }
                MONITOR.with(|m| m.set((ea, total, lo, hi)));
            } else {
                let (mon_ea, mon_len, lo, hi) = MONITOR.with(|m| m.replace((0, 0, 0, 0)));
                let o = if *ord {
                    Ordering::Release
                } else {
                    Ordering::Relaxed
                };
                let ok = mon_ea == ea && mon_len == total && {
                    // SAFETY: per the caller.
                    unsafe {
                        if *pair && *size == 8 {
                            cas16(p, (lo, hi), (r.gpr(*rt), r.gpr(*rt2))) == (lo, hi)
                        } else if *pair {
                            let e = mask(4, lo) | mask(4, hi) << 32;
                            let n = mask(4, r.gpr(*rt)) | mask(4, r.gpr(*rt2)) << 32;
                            cas(p, 8, e, n, o) == e
                        } else {
                            let n = mask(*size, r.gpr(*rt));
                            cas(p, *size, lo, n, o) == lo
                        }
                    }
                };
                set(*rs, u64::from(!ok));
            }
            true
        }
        Op::Lane {
            load,
            size,
            index,
            rt,
            rn,
            post,
        } => {
            let n = usize::from(*size);
            let at = usize::from(*index) * n;
            let reg = usize::from(*rt);
            // SAFETY: per the caller.
            unsafe {
                let mut b = r.v.add(reg).read_unaligned().to_le_bytes();
                if *load {
                    for (j, d) in b[at..at + n].iter_mut().enumerate() {
                        *d = p.add(j).read_volatile();
                    }
                    r.v.add(reg).write_unaligned(u128::from_le_bytes(b));
                } else {
                    for (j, sv) in b[at..at + n].iter().enumerate() {
                        p.add(j).write_volatile(*sv);
                    }
                }
                if let (Some(w), Some(_)) = (plan.writeback, post) {
                    if *rn >= 31 {
                        r.sp.write(w);
                    } else {
                        r.x.add(usize::from(*rn)).write(w);
                    }
                }
            }
            true
        }
        Op::Ld1 {
            load, q, regs, rt, ..
        } => {
            let n = if *q { 16 } else { 8 };
            for k in 0..usize::from(*regs) {
                let reg = (usize::from(*rt) + k) % 32;
                // SAFETY: per the caller.
                unsafe {
                    let at = p.add(k * n);
                    if *load {
                        let mut b = [0u8; 16];
                        for (j, d) in b.iter_mut().take(n).enumerate() {
                            *d = at.add(j).read_volatile();
                        }
                        r.v.add(reg).write_unaligned(u128::from_le_bytes(b));
                    } else {
                        let b = r.v.add(reg).read_unaligned().to_le_bytes();
                        for (j, s) in b.iter().take(n).enumerate() {
                            at.add(j).write_volatile(*s);
                        }
                    }
                }
            }
            if let Some(w) = plan.writeback {
                // SAFETY: per the caller.
                unsafe {
                    match op {
                        Op::Ld1 { rn, .. } if *rn >= 31 => r.sp.write(w),
                        Op::Ld1 { rn, .. } => r.x.add(usize::from(*rn)).write(w),
                        _ => {}
                    }
                }
            }
            true
        }
    }
}
