//! The loads and stores that the page tracker emulates (`NXKIT_PRIVATE=1`, "Private GPU memory, step 4" in
//! `docs/nxkit-gpu.md`).
//!
//! A CPU access to a page that a frame in flight writes faults. When the access lies outside the bytes the frame may
//! write, the handler does not wait for the frame: it decodes the faulting instruction ([`decode`]), computes the
//! address it uses ([`plan`]), and does the access through a second mapping of guest memory ([`perform`]). Loads fill
//! the destination registers of the saved thread state, stores read the source registers, and the program counter
//! moves by 4.
//!
//! The decoder knows the integer and SIMD loads and stores that compilers and the guest's JIT emit:
//!
//! | form | encodings |
//! |---|---|
//! | `Ldr`, `Str` | `LDR`/`STR` of `b`, `h`, `w`, `x` with an unsigned offset, a pre or post index, a register offset (`UXTW`, `LSL`, `SXTW`, `SXTX`, optional shift), and `LDUR`/`STUR`; `LDRB`, `LDRH`, `STRB`, `STRH` |
//! | `Ldrs` | `LDRSB`, `LDRSH`, `LDRSW` into `w` or `x`, all the addressing modes above |
//! | `Ldp`, `Stp`, `Ldpsw` | `LDP`/`STP` of `w` or `x`, `LDPSW`, signed offset, pre and post index, and `LDNP`/`STNP` |
//! | `VLdr`, `VStr`, `VLdp`, `VStp` | the same for `b`, `h`, `s`, `d`, `q` registers |
//! | `Ldar`, `Stlr` | `LDAR`, `LDARB`, `LDARH`, `STLR`, `STLRB`, `STLRH`, and `LDAPR` (as an acquire load) |
//!
//! Anything else gets a [`Reason`] (exclusives, atomics of the large system extensions, `LD1`/`ST1`, `DC ZVA`,
//! prefetches, literal loads, unprivileged accesses) and the caller falls back to the wait.
//!
//! The executor works on a [`Regs`] of raw pointers, so the handler passes the saved thread state and the tests pass a
//! local array. Aligned accesses of 1, 2, 4 and 8 bytes are single atomic accesses (relaxed, acquire for `LDAR`,
//! release for `STLR`); a 16-byte access is two 8-byte ones; a misaligned access copies bytes.

use std::sync::atomic::{AtomicU8, AtomicU16, AtomicU32, AtomicU64, Ordering};

/// What an emulated access was, for the counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// `LDR`, `LDUR`, `LDRB`, `LDRH` into a general register.
    Ldr = 0,
    /// `LDRSB`, `LDRSH`, `LDRSW`.
    Ldrs = 1,
    /// `LDP`, `LDNP` of general registers.
    Ldp = 2,
    /// `LDPSW`.
    Ldpsw = 3,
    /// `LDAR`, `LDARB`, `LDARH`, `LDAPR`.
    Ldar = 4,
    /// `LDR` into a SIMD register.
    VLdr = 5,
    /// `LDP`, `LDNP` of SIMD registers.
    VLdp = 6,
    /// `STR`, `STUR`, `STRB`, `STRH` from a general register.
    Str = 7,
    /// `STP`, `STNP` of general registers.
    Stp = 8,
    /// `STLR`, `STLRB`, `STLRH`.
    Stlr = 9,
    /// `STR` from a SIMD register.
    VStr = 10,
    /// `STP`, `STNP` of SIMD registers.
    VStp = 11,
}

impl Form {
    /// The number of forms.
    pub const COUNT: usize = 12;

    /// The forms in the order of their numbers.
    pub const ALL: [Form; Form::COUNT] = [
        Form::Ldr,
        Form::Ldrs,
        Form::Ldp,
        Form::Ldpsw,
        Form::Ldar,
        Form::VLdr,
        Form::VLdp,
        Form::Str,
        Form::Stp,
        Form::Stlr,
        Form::VStr,
        Form::VStp,
    ];

    /// The name for reports.
    pub fn name(self) -> &'static str {
        match self {
            Form::Ldr => "ldr",
            Form::Ldrs => "ldrs",
            Form::Ldp => "ldp",
            Form::Ldpsw => "ldpsw",
            Form::Ldar => "ldar",
            Form::VLdr => "simd ldr",
            Form::VLdp => "simd ldp",
            Form::Str => "str",
            Form::Stp => "stp",
            Form::Stlr => "stlr",
            Form::VStr => "simd str",
            Form::VStp => "simd stp",
        }
    }

    /// True for the loads.
    pub fn is_load(self) -> bool {
        (self as usize) < Form::Str as usize
    }
}

/// Why an access is not emulated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// `LDXR`, `STXR`, `LDAXR`, `STLXR`, `LDXP`, `STXP`.
    Exclusive = 0,
    /// The atomic memory operations of the large system extensions: `LDADD`, `SWP`, `CAS` and the others.
    Atomic = 1,
    /// `LD1` to `LD4`, `ST1` to `ST4` (structure loads and stores of SIMD registers).
    Structure = 2,
    /// `DC ZVA`.
    Zva = 3,
    /// `PRFM`, `PRFUM`.
    Prefetch = 4,
    /// A literal load, an unprivileged access, a tag or pointer-authenticated access, anything not listed.
    Other = 5,
    /// The access touches bytes that the frame may write.
    Overlap = 6,
    /// The access crosses the border of the page, or the instruction names an address that the fault did not.
    Mismatch = 7,
}

impl Reason {
    /// The number of reasons.
    pub const COUNT: usize = 8;

    /// The reasons in the order of their numbers.
    pub const ALL: [Reason; Reason::COUNT] = [
        Reason::Exclusive,
        Reason::Atomic,
        Reason::Structure,
        Reason::Zva,
        Reason::Prefetch,
        Reason::Other,
        Reason::Overlap,
        Reason::Mismatch,
    ];

    /// The name for reports.
    pub fn name(self) -> &'static str {
        match self {
            Reason::Exclusive => "exclusive",
            Reason::Atomic => "atomic",
            Reason::Structure => "ld1/st1",
            Reason::Zva => "dc zva",
            Reason::Prefetch => "prefetch",
            Reason::Other => "other",
            Reason::Overlap => "touches written bytes",
            Reason::Mismatch => "page border or address mismatch",
        }
    }
}

/// How the address is formed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// The base plus an offset; the base register is not changed.
    Offset(i64),
    /// The base plus an offset, which is also written back to the base.
    Pre(i64),
    /// The base; the base plus an offset is written back after the access.
    Post(i64),
    /// The base plus a register, extended by `option` (2 `UXTW`, 3 `LSL`, 6 `SXTW`, 7 `SXTX`) and shifted left.
    Reg {
        /// The offset register.
        rm: u8,
        /// The extension.
        option: u8,
        /// The left shift after the extension.
        shift: u8,
    },
}

/// The memory order of an access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Order {
    /// No order.
    Plain,
    /// Load-acquire.
    Acquire,
    /// Store-release.
    Release,
}

/// A decoded load or store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Mem {
    /// The form, for the counters.
    pub(crate) form: Form,
    /// True for a load.
    pub(crate) load: bool,
    /// The registers are SIMD registers.
    pub(crate) simd: bool,
    /// Two registers.
    pub(crate) pair: bool,
    /// Bytes per register: 1, 2, 4, 8 or 16.
    pub(crate) size: u8,
    /// A load that sign-extends.
    pub(crate) signed: bool,
    /// The destination of a sign-extending load is an `x` register (else a `w` register).
    pub(crate) dest64: bool,
    /// The (first) data register.
    pub(crate) rt: u8,
    /// The second data register of a pair.
    pub(crate) rt2: u8,
    /// The base register (31 is the stack pointer).
    pub(crate) rn: u8,
    /// The address.
    pub(crate) mode: Mode,
    /// The order.
    pub(crate) order: Order,
}

fn sext(v: u32, bits: u32) -> i64 {
    let s = 64 - bits;
    ((u64::from(v) << s) as i64) >> s
}

/// Decodes the instruction word `i`.
pub(crate) fn decode(i: u32) -> Result<Mem, Reason> {
    // `DC ZVA, Xt` is `SYS #3, C7, C4, #1, Xt`.
    if i & 0xffff_ffe0 == 0xd50b_7420 {
        return Err(Reason::Zva);
    }
    // Loads and stores: bit 27 set, bit 25 clear.
    if i & 0x0a00_0000 != 0x0800_0000 {
        return Err(Reason::Other);
    }
    // `LD1`..`LD4`, `ST1`..`ST4`, multiple and single structures.
    if i & 0xbe00_0000 == 0x0c00_0000 {
        return Err(Reason::Structure);
    }
    let rt = (i & 31) as u8;
    let rn = ((i >> 5) & 31) as u8;
    let size = ((i >> 30) & 3) as u8;
    let v = (i >> 26) & 1 != 0;
    let opc = ((i >> 22) & 3) as u8;
    // Literal loads.
    if i & 0x3b00_0000 == 0x1800_0000 {
        return Err(Reason::Other);
    }
    {
        // Exclusive and ordered.
        if i & 0x3f00_0000 == 0x0800_0000 {
            let o2 = i >> 23 & 1;
            let l = i >> 22 & 1;
            let o1 = i >> 21 & 1;
            let o0 = i >> 15 & 1;
            if o2 == 0 {
                return Err(Reason::Exclusive);
            }
            if o1 == 1 {
                return Err(Reason::Atomic);
            }
            // `LDAR`, `STLR`: Rs and Rt2 are all ones.
            if o0 == 1 && (i >> 16) & 31 == 31 && (i >> 10) & 31 == 31 {
                let load = l == 1;
                return Ok(Mem {
                    form: if load { Form::Ldar } else { Form::Stlr },
                    load,
                    simd: false,
                    pair: false,
                    size: 1 << size,
                    signed: false,
                    dest64: size == 3,
                    rt,
                    rt2: 0,
                    rn,
                    mode: Mode::Offset(0),
                    order: if load { Order::Acquire } else { Order::Release },
                });
            }
            return Err(Reason::Other);
        }
    }
    match (i >> 27) & 7 {
        // Pairs: `opc 101 V type L imm7 Rt2 Rn Rt`.
        0b101 => {
            let ty = (i >> 23) & 3;
            let load = (i >> 22) & 1 == 1;
            let rt2 = ((i >> 10) & 31) as u8;
            let opc = i >> 30;
            let (bytes, signed, simd) = match (v, opc) {
                (false, 0) => (4u8, false, false),
                (false, 1) if load => (4, true, false),
                (false, 2) => (8, false, false),
                (true, 0) => (4, false, true),
                (true, 1) => (8, false, true),
                (true, 2) => (16, false, true),
                _ => return Err(Reason::Other),
            };
            let scale = bytes.trailing_zeros();
            let imm = sext((i >> 15) & 0x7f, 7) << scale;
            let mode = match ty {
                0 | 2 => Mode::Offset(imm),
                1 => Mode::Post(imm),
                _ => Mode::Pre(imm),
            };
            let form = match (load, simd, signed) {
                (true, false, false) => Form::Ldp,
                (true, false, true) => Form::Ldpsw,
                (true, true, _) => Form::VLdp,
                (false, false, _) => Form::Stp,
                (false, true, _) => Form::VStp,
            };
            Ok(Mem {
                form,
                load,
                simd,
                pair: true,
                size: bytes,
                signed,
                dest64: signed || bytes == 8,
                rt,
                rt2,
                rn,
                mode,
                order: Order::Plain,
            })
        }
        // Single registers.
        0b111 => {
            let kind = (i >> 24) & 3;
            let b21 = (i >> 21) & 1;
            let sub = (i >> 10) & 3;
            if kind == 0 && b21 == 1 && sub == 0 {
                // The group of the atomic memory operations. `LDAPR` is the one load here.
                let is_ldapr = !v && opc == 2 && (i >> 16) & 31 == 31 && (i >> 12) & 15 == 0b1100;
                if is_ldapr {
                    return Ok(Mem {
                        form: Form::Ldar,
                        load: true,
                        simd: false,
                        pair: false,
                        size: 1 << size,
                        signed: false,
                        dest64: size == 3,
                        rt,
                        rt2: 0,
                        rn,
                        mode: Mode::Offset(0),
                        order: Order::Acquire,
                    });
                }
                return Err(if v { Reason::Other } else { Reason::Atomic });
            }
            // The data: bytes, simd, load, signed, dest64.
            let (bytes, simd, load, signed, dest64) = if !v {
                match opc {
                    0 => (1u8 << size, false, false, false, size == 3),
                    1 => (1 << size, false, true, false, size == 3),
                    2 if size == 3 => return Err(Reason::Prefetch),
                    2 => (1 << size, false, true, true, true),
                    _ if size >= 2 => return Err(Reason::Other),
                    _ => (1 << size, false, true, true, false),
                }
            } else {
                match (size, opc) {
                    (s, 0) => (1 << s, true, false, false, false),
                    (s, 1) => (1 << s, true, true, false, false),
                    (0, 2) => (16, true, false, false, false),
                    (0, 3) => (16, true, true, false, false),
                    _ => return Err(Reason::Other),
                }
            };
            let scale = bytes.trailing_zeros();
            let mode = match kind {
                1 => Mode::Offset(i64::from((i >> 10) & 0xfff) << scale),
                0 if b21 == 0 => {
                    let imm = sext((i >> 12) & 0x1ff, 9);
                    match sub {
                        0 => Mode::Offset(imm),
                        1 => Mode::Post(imm),
                        3 => Mode::Pre(imm),
                        // Unprivileged (`LDTR`, `STTR`).
                        _ => return Err(Reason::Other),
                    }
                }
                0 if sub == 2 => {
                    let option = ((i >> 13) & 7) as u8;
                    if !matches!(option, 2 | 3 | 6 | 7) {
                        return Err(Reason::Other);
                    }
                    Mode::Reg {
                        rm: ((i >> 16) & 31) as u8,
                        option,
                        shift: if (i >> 12) & 1 == 1 { scale as u8 } else { 0 },
                    }
                }
                _ => return Err(Reason::Other),
            };
            let form = match (load, simd, signed) {
                (true, false, false) => Form::Ldr,
                (true, false, true) => Form::Ldrs,
                (true, true, _) => Form::VLdr,
                (false, false, _) => Form::Str,
                (false, true, _) => Form::VStr,
            };
            Ok(Mem {
                form,
                load,
                simd,
                pair: false,
                size: bytes,
                signed,
                dest64,
                rt,
                rt2: 0,
                rn,
                mode,
                order: Order::Plain,
            })
        }
        _ => Err(Reason::Other),
    }
}

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
    unsafe fn gpr(&self, n: u8) -> u64 {
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
    unsafe fn base(&self, n: u8) -> u64 {
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
unsafe fn read(p: *const u8, n: usize, order: Ordering) -> u128 {
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
                    val = u128::from(if op.dest64 { ext as u64 } else { u64::from(ext as u32) });
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

#[cfg(test)]
mod tests {
    use super::*;

    fn d(i: u32) -> Mem {
        decode(i).unwrap_or_else(|e| panic!("{i:#010x} not decoded: {e:?}"))
    }

    #[allow(clippy::too_many_arguments)]
    fn check(
        i: u32,
        form: Form,
        size: u8,
        rt: u8,
        rt2: u8,
        rn: u8,
        mode: Mode,
        signed: bool,
        dest64: bool,
    ) {
        let m = d(i);
        assert_eq!(m.form, form, "{i:#010x}");
        assert_eq!(m.load, form.is_load(), "{i:#010x}");
        assert_eq!(m.size, size, "{i:#010x}");
        assert_eq!((m.rt, m.rn), (rt, rn), "{i:#010x}");
        if m.pair {
            assert_eq!(m.rt2, rt2, "{i:#010x}");
        }
        assert_eq!(m.mode, mode, "{i:#010x}");
        assert_eq!(m.signed, signed, "{i:#010x}");
        if signed {
            assert_eq!(m.dest64, dest64, "{i:#010x}");
        }
    }

    use Form::*;
    use Mode::*;

    #[test]
    fn unsigned_offset_loads_and_stores() {
        check(0xf940_0841, Ldr, 8, 1, 0, 2, Offset(16), false, false);
        check(0xb940_0c41, Ldr, 4, 1, 0, 2, Offset(12), false, false);
        check(0xf940_07e1, Ldr, 8, 1, 0, 31, Offset(8), false, false);
        check(0x3940_1483, Ldr, 1, 3, 0, 4, Offset(5), false, false);
        check(0x7940_0c83, Ldr, 2, 3, 0, 4, Offset(6), false, false);
        check(0x3980_1483, Ldrs, 1, 3, 0, 4, Offset(5), true, true);
        check(0x39c0_1483, Ldrs, 1, 3, 0, 4, Offset(5), true, false);
        check(0x7980_0c83, Ldrs, 2, 3, 0, 4, Offset(6), true, true);
        check(0x79c0_0c83, Ldrs, 2, 3, 0, 4, Offset(6), true, false);
        check(0xb980_0883, Ldrs, 4, 3, 0, 4, Offset(8), true, true);
        check(0xf900_0841, Str, 8, 1, 0, 2, Offset(16), false, false);
        check(0xb900_0c41, Str, 4, 1, 0, 2, Offset(12), false, false);
        check(0xf900_0c5f, Str, 8, 31, 0, 2, Offset(24), false, false);
        check(0x3900_1483, Str, 1, 3, 0, 4, Offset(5), false, false);
        check(0x7900_0c83, Str, 2, 3, 0, 4, Offset(6), false, false);
    }

    #[test]
    fn unscaled_offsets() {
        check(0xf85f_8041, Ldr, 8, 1, 0, 2, Offset(-8), false, false);
        check(0xb840_3041, Ldr, 4, 1, 0, 2, Offset(3), false, false);
        check(0x385f_f041, Ldr, 1, 1, 0, 2, Offset(-1), false, false);
        check(0x785f_e041, Ldr, 2, 1, 0, 2, Offset(-2), false, false);
        check(0x389f_f041, Ldrs, 1, 1, 0, 2, Offset(-1), true, true);
        check(0x78df_e041, Ldrs, 2, 1, 0, 2, Offset(-2), true, false);
        check(0xb89f_c041, Ldrs, 4, 1, 0, 2, Offset(-4), true, true);
        check(0xf81f_8041, Str, 8, 1, 0, 2, Offset(-8), false, false);
        check(0x381f_f041, Str, 1, 1, 0, 2, Offset(-1), false, false);
        check(0x781f_e041, Str, 2, 1, 0, 2, Offset(-2), false, false);
        check(0xb81f_c041, Str, 4, 1, 0, 2, Offset(-4), false, false);
    }

    #[test]
    fn pre_and_post_index() {
        check(0xf841_0c41, Ldr, 8, 1, 0, 2, Pre(16), false, false);
        check(0xb85f_4c41, Ldr, 4, 1, 0, 2, Pre(-12), false, false);
        check(0xf841_0441, Ldr, 8, 1, 0, 2, Post(16), false, false);
        check(0xb85f_c441, Ldr, 4, 1, 0, 2, Post(-4), false, false);
        check(0xf81f_0c41, Str, 8, 1, 0, 2, Pre(-16), false, false);
        check(0xf800_8441, Str, 8, 1, 0, 2, Post(8), false, false);
        check(0x3800_1441, Str, 1, 1, 0, 2, Post(1), false, false);
        check(0x3840_1c41, Ldr, 1, 1, 0, 2, Pre(1), false, false);
    }

    #[test]
    fn register_offsets() {
        check(0xf863_6841, Ldr, 8, 1, 0, 2, Reg { rm: 3, option: 3, shift: 0 }, false, false);
        check(0xf863_7841, Ldr, 8, 1, 0, 2, Reg { rm: 3, option: 3, shift: 3 }, false, false);
        check(0xb863_5841, Ldr, 4, 1, 0, 2, Reg { rm: 3, option: 2, shift: 2 }, false, false);
        check(0xb863_c841, Ldr, 4, 1, 0, 2, Reg { rm: 3, option: 6, shift: 0 }, false, false);
        check(0xf863_d841, Ldr, 8, 1, 0, 2, Reg { rm: 3, option: 6, shift: 3 }, false, false);
        check(0xf863_f841, Ldr, 8, 1, 0, 2, Reg { rm: 3, option: 7, shift: 3 }, false, false);
        check(0x3863_6841, Ldr, 1, 1, 0, 2, Reg { rm: 3, option: 3, shift: 0 }, false, false);
        check(0x3863_4841, Ldr, 1, 1, 0, 2, Reg { rm: 3, option: 2, shift: 0 }, false, false);
        check(0x7863_7841, Ldr, 2, 1, 0, 2, Reg { rm: 3, option: 3, shift: 1 }, false, false);
        check(0xb8a3_7841, Ldrs, 4, 1, 0, 2, Reg { rm: 3, option: 3, shift: 2 }, true, true);
        check(0x38e3_6841, Ldrs, 1, 1, 0, 2, Reg { rm: 3, option: 3, shift: 0 }, true, false);
        check(0x78a3_7841, Ldrs, 2, 1, 0, 2, Reg { rm: 3, option: 3, shift: 1 }, true, true);
        check(0xf823_7841, Str, 8, 1, 0, 2, Reg { rm: 3, option: 3, shift: 3 }, false, false);
        check(0xb823_5841, Str, 4, 1, 0, 2, Reg { rm: 3, option: 2, shift: 2 }, false, false);
        check(0x3823_6841, Str, 1, 1, 0, 2, Reg { rm: 3, option: 3, shift: 0 }, false, false);
        check(0x7823_7841, Str, 2, 1, 0, 2, Reg { rm: 3, option: 3, shift: 1 }, false, false);
    }

    #[test]
    fn pairs() {
        check(0xa941_0861, Ldp, 8, 1, 2, 3, Offset(16), false, false);
        check(0x2941_0861, Ldp, 4, 1, 2, 3, Offset(8), false, false);
        check(0xa9ff_0861, Ldp, 8, 1, 2, 3, Pre(-16), false, false);
        check(0xa8c1_0861, Ldp, 8, 1, 2, 3, Post(16), false, false);
        check(0xa901_0861, Stp, 8, 1, 2, 3, Offset(16), false, false);
        check(0x29bf_0be1, Stp, 4, 1, 2, 31, Pre(-8), false, false);
        check(0xa8be_0861, Stp, 8, 1, 2, 3, Post(-32), false, false);
        check(0xa900_fc7f, Stp, 8, 31, 31, 3, Offset(8), false, false);
        check(0x6941_0861, Ldpsw, 4, 1, 2, 3, Offset(8), true, true);
        check(0x68c1_0861, Ldpsw, 4, 1, 2, 3, Post(8), true, true);
        check(0x69ff_0861, Ldpsw, 4, 1, 2, 3, Pre(-8), true, true);
        check(0xa841_0861, Ldp, 8, 1, 2, 3, Offset(16), false, false);
        check(0xa801_0861, Stp, 8, 1, 2, 3, Offset(16), false, false);
    }

    #[test]
    fn simd_single_registers() {
        check(0x3d40_0c41, VLdr, 1, 1, 0, 2, Offset(3), false, false);
        check(0x7d40_0c41, VLdr, 2, 1, 0, 2, Offset(6), false, false);
        check(0xbd40_0c41, VLdr, 4, 1, 0, 2, Offset(12), false, false);
        check(0xfd40_0841, VLdr, 8, 1, 0, 2, Offset(16), false, false);
        check(0x3dc0_0841, VLdr, 16, 1, 0, 2, Offset(32), false, false);
        check(0x3d00_0c41, VStr, 1, 1, 0, 2, Offset(3), false, false);
        check(0x7d00_0c41, VStr, 2, 1, 0, 2, Offset(6), false, false);
        check(0xbd00_0c41, VStr, 4, 1, 0, 2, Offset(12), false, false);
        check(0xfd00_0841, VStr, 8, 1, 0, 2, Offset(16), false, false);
        check(0x3d80_0841, VStr, 16, 1, 0, 2, Offset(32), false, false);
        check(0x3c5f_f041, VLdr, 1, 1, 0, 2, Offset(-1), false, false);
        check(0x3cde_f041, VLdr, 16, 1, 0, 2, Offset(-17), false, false);
        check(0xfc1f_7041, VStr, 8, 1, 0, 2, Offset(-9), false, false);
        check(0x3c9f_0041, VStr, 16, 1, 0, 2, Offset(-16), false, false);
        check(0x3cc1_0c41, VLdr, 16, 1, 0, 2, Pre(16), false, false);
        check(0x3cc1_0441, VLdr, 16, 1, 0, 2, Post(16), false, false);
        check(0x3c9f_0c41, VStr, 16, 1, 0, 2, Pre(-16), false, false);
        check(0xfc00_8441, VStr, 8, 1, 0, 2, Post(8), false, false);
        check(0x3ce3_7841, VLdr, 16, 1, 0, 2, Reg { rm: 3, option: 3, shift: 4 }, false, false);
        check(0xfc63_7841, VLdr, 8, 1, 0, 2, Reg { rm: 3, option: 3, shift: 3 }, false, false);
        check(0xbc63_5841, VLdr, 4, 1, 0, 2, Reg { rm: 3, option: 2, shift: 2 }, false, false);
        check(0x3c63_6841, VLdr, 1, 1, 0, 2, Reg { rm: 3, option: 3, shift: 0 }, false, false);
        check(0x3ca3_7841, VStr, 16, 1, 0, 2, Reg { rm: 3, option: 3, shift: 4 }, false, false);
        check(0x7c23_7841, VStr, 2, 1, 0, 2, Reg { rm: 3, option: 3, shift: 1 }, false, false);
    }

    #[test]
    fn simd_pairs() {
        check(0x2d41_0861, VLdp, 4, 1, 2, 3, Offset(8), false, false);
        check(0x6d41_0861, VLdp, 8, 1, 2, 3, Offset(16), false, false);
        check(0xad41_0861, VLdp, 16, 1, 2, 3, Offset(32), false, false);
        check(0xadbf_0861, VStp, 16, 1, 2, 3, Pre(-32), false, false);
        check(0x6c81_0861, VStp, 8, 1, 2, 3, Post(16), false, false);
        check(0x2d00_8861, VStp, 4, 1, 2, 3, Offset(4), false, false);
        check(0xac41_0861, VLdp, 16, 1, 2, 3, Offset(32), false, false);
        check(0x6c02_0861, VStp, 8, 1, 2, 3, Offset(32), false, false);
    }

    #[test]
    fn ordered_accesses() {
        for (i, form, size, order) in [
            (0xc8df_fc41, Ldar, 8, Order::Acquire),
            (0x88df_fc41, Ldar, 4, Order::Acquire),
            (0x08df_fc41, Ldar, 1, Order::Acquire),
            (0x48df_fc41, Ldar, 2, Order::Acquire),
            (0xc89f_fc41, Stlr, 8, Order::Release),
            (0x889f_fc41, Stlr, 4, Order::Release),
            (0x089f_fc41, Stlr, 1, Order::Release),
            (0x489f_fc41, Stlr, 2, Order::Release),
            (0xf8bf_c041, Ldar, 8, Order::Acquire),
            (0xb8bf_c041, Ldar, 4, Order::Acquire),
            (0x38bf_c041, Ldar, 1, Order::Acquire),
            (0x78bf_c041, Ldar, 2, Order::Acquire),
        ] {
            let m = d(i);
            assert_eq!((m.form, m.size, m.order), (form, size, order), "{i:#010x}");
            assert_eq!((m.rt, m.rn, m.mode), (1, 2, Offset(0)), "{i:#010x}");
        }
    }

    #[test]
    fn what_is_not_emulated_has_a_reason() {
        for (i, why) in [
            (0xc85f_7c41, Reason::Exclusive),
            (0xc803_7c41, Reason::Exclusive),
            (0xc85f_fc41, Reason::Exclusive),
            (0xc803_fc41, Reason::Exclusive),
            (0xc87f_0861, Reason::Exclusive),
            (0xf821_0062, Reason::Atomic),
            (0xf821_8082, Reason::Atomic),
            (0xc8a1_7c62, Reason::Atomic),
            (0x88e1_fc62, Reason::Atomic),
            (0xf8e1_0062, Reason::Atomic),
            (0xf980_0440, Reason::Prefetch),
            (0xf8a3_6840, Reason::Prefetch),
            (0xf89f_8040, Reason::Prefetch),
            (0x5800_0021, Reason::Other),
            (0x9c00_0001, Reason::Other),
            (0xf840_8841, Reason::Other),
            (0xf800_8841, Reason::Other),
            (0x4c40_7020, Reason::Structure),
            (0x4c00_a820, Reason::Structure),
            (0x0d40_9020, Reason::Structure),
            (0x4d40_c820, Reason::Structure),
            (0x4cdf_7020, Reason::Structure),
            (0xd50b_7421, Reason::Zva),
            (0x8b03_0041, Reason::Other),
        ] {
            assert_eq!(decode(i).err(), Some(why), "{i:#010x}");
        }
    }

    /// A register file and a 64-byte buffer for the executor tests.
    struct Rig {
        x: [u64; 31],
        sp: u64,
        v: [u128; 32],
        mem: [u8; 128],
    }

    impl Rig {
        fn new() -> Self {
            let mut x = [0u64; 31];
            for (k, r) in x.iter_mut().enumerate() {
                *r = 0x1111_1111_1111_1111u64.wrapping_mul(k as u64 + 1) / 3;
            }
            let mut v = [0u128; 32];
            for (k, r) in v.iter_mut().enumerate() {
                *r = (k as u128 + 1) * 0x0101_0101_0101_0101_0101_0101_0101_0101;
            }
            let mut mem = [0u8; 128];
            for (k, b) in mem.iter_mut().enumerate() {
                *b = k as u8 ^ 0x5a;
            }
            Self { x, sp: 0, v, mem }
        }

        fn regs(&mut self) -> Regs {
            Regs {
                x: self.x.as_mut_ptr(),
                sp: &raw mut self.sp,
                v: self.v.as_mut_ptr(),
            }
        }

        /// Runs the instruction with the base register pointing at byte `at` of `mem`.
        fn run(&mut self, i: u32, at: usize) -> Plan {
            let op = d(i);
            let base = self.mem.as_mut_ptr() as u64 + at as u64;
            if op.rn == 31 {
                self.sp = base;
            } else {
                self.x[usize::from(op.rn)] = base;
            }
            let r = self.regs();
            // SAFETY: the register file and the buffer are the rig's own; the access lies inside the buffer.
            unsafe {
                let p = plan(&op, &r);
                let off = p.ea.wrapping_sub(self.mem.as_ptr() as u64) as usize;
                perform(&op, &r, &p, self.mem.as_mut_ptr().add(off));
                p
            }
        }

        fn word(&self, at: usize, n: usize) -> u64 {
            let mut b = [0u8; 8];
            b[..n].copy_from_slice(&self.mem[at..at + n]);
            u64::from_le_bytes(b)
        }
    }

    #[test]
    fn loads_extend_and_fill_registers() {
        let mut r = Rig::new();
        let w = r.word(16, 8);
        r.run(0xf940_0841, 0); // ldr x1, [x2, #16]
        assert_eq!(r.x[1], w);
        r.run(0xb940_0c41, 0); // ldr w1, [x2, #12]
        assert_eq!(r.x[1], r.word(12, 4));
        r.x[3] = u64::MAX;
        r.run(0x3940_1483, 0); // ldrb w3, [x4, #5]
        assert_eq!(r.x[3], r.word(5, 1));
        r.run(0x7940_0c83, 0); // ldrh w3, [x4, #6]
        assert_eq!(r.x[3], r.word(6, 2));
        r.mem[5] = 0x80;
        r.mem[6] = 0x00;
        r.mem[7] = 0x80;
        r.run(0x3980_1483, 0); // ldrsb x3, [x4, #5]
        assert_eq!(r.x[3], 0xffff_ffff_ffff_ff80);
        r.run(0x39c0_1483, 0); // ldrsb w3, [x4, #5]
        assert_eq!(r.x[3], 0x0000_0000_ffff_ff80);
        r.run(0x7980_0c83, 0); // ldrsh x3, [x4, #6]
        assert_eq!(r.x[3], 0xffff_ffff_ffff_8000 | u64::from(r.mem[6]));
        r.mem[8..12].copy_from_slice(&0x8000_0001u32.to_le_bytes());
        r.run(0xb980_0883, 0); // ldrsw x3, [x4, #8]
        assert_eq!(r.x[3], 0xffff_ffff_8000_0001);
        r.x[1] = 7;
        r.run(0xf940_0841 | 31, 0); // ldr xzr, [x2, #16]: no register changes
        assert_eq!(r.x[1], 7);
    }

    #[test]
    fn stores_write_the_low_bytes_and_xzr_stores_zero() {
        let mut r = Rig::new();
        r.x[1] = 0x0102_0304_0506_0708;
        r.run(0xf900_0841, 0); // str x1, [x2, #16]
        assert_eq!(r.word(16, 8), 0x0102_0304_0506_0708);
        r.run(0xb900_0c41, 0); // str w1, [x2, #12]
        assert_eq!(r.word(12, 4), 0x0506_0708);
        r.x[3] = 0x1ff;
        r.run(0x3900_1483, 0); // strb w3, [x4, #5]
        assert_eq!(r.mem[5], 0xff);
        assert_eq!(r.mem[6], 6 ^ 0x5a, "the next byte is not touched");
        r.run(0xf900_0c5f, 0); // str xzr, [x2, #24]
        assert_eq!(r.word(24, 8), 0);
        r.x[3] = 0xaabb_ccdd_eeff_1122;
        r.run(0x7900_0c83, 0); // strh w3, [x4, #6]
        assert_eq!(r.word(6, 2), 0x1122);
    }

    #[test]
    fn the_base_register_is_written_back_for_pre_and_post_index() {
        let mut r = Rig::new();
        let base = r.mem.as_ptr() as u64;
        let p = r.run(0xf841_0c41, 8); // ldr x1, [x2, #16]!
        assert_eq!(p.ea, base + 24);
        assert_eq!(r.x[2], base + 24);
        assert_eq!(r.x[1], r.word(24, 8));
        let p = r.run(0xf841_0441, 8); // ldr x1, [x2], #16
        assert_eq!(p.ea, base + 8);
        assert_eq!(r.x[2], base + 24);
        assert_eq!(r.x[1], r.word(8, 8));
        let p = r.run(0x29bf_0be1, 40); // stp w1, w2, [sp, #-8]!
        assert_eq!(p.ea, base + 32);
        assert_eq!(r.sp, base + 32);
        let (a, b) = (r.x[1] as u32, r.x[2] as u32);
        assert_eq!(r.word(32, 4), u64::from(a));
        assert_eq!(r.word(36, 4), u64::from(b));
    }

    #[test]
    fn register_offsets_extend_and_shift() {
        let mut r = Rig::new();
        let base = r.mem.as_ptr() as u64;
        // ldr x1, [x2, x3, lsl #3]
        r.x[3] = 4;
        let op = d(0xf863_7841);
        r.x[2] = base;
        let rg = r.regs();
        // SAFETY: the rig's own register file.
        let p = unsafe { plan(&op, &rg) };
        assert_eq!(p.ea, base + 32);
        // ldr w1, [x2, w3, uxtw #2] with the upper half of x3 set: it is ignored.
        r.x[3] = 0xffff_ffff_0000_0003;
        let op = d(0xb863_5841);
        let rg = r.regs();
        // SAFETY: as above.
        let p = unsafe { plan(&op, &rg) };
        assert_eq!(p.ea, base + 12);
        // ldr w1, [x2, w3, sxtw]
        r.x[3] = 0x0000_0000_ffff_fffc;
        r.x[2] = base + 16;
        let op = d(0xb863_c841);
        let rg = r.regs();
        // SAFETY: as above.
        let p = unsafe { plan(&op, &rg) };
        assert_eq!(p.ea, base + 12);
        // ldr x1, [x2, x3, sxtx #3]
        r.x[3] = (-2i64) as u64;
        r.x[2] = base + 32;
        let op = d(0xf863_f841);
        let rg = r.regs();
        // SAFETY: as above.
        let p = unsafe { plan(&op, &rg) };
        assert_eq!(p.ea, base + 16);
        // A zero register as offset adds nothing: ldr x1, [x2, xzr]
        let op = d(0xf87f_6841);
        let rg = r.regs();
        // SAFETY: as above.
        let p = unsafe { plan(&op, &rg) };
        assert_eq!(p.ea, base + 32);
    }

    #[test]
    fn pairs_load_and_store_both_registers() {
        let mut r = Rig::new();
        r.run(0xa941_0861, 0); // ldp x1, x2, [x3, #16]
        assert_eq!((r.x[1], r.x[2]), (r.word(16, 8), r.word(24, 8)));
        r.run(0x2941_0861, 0); // ldp w1, w2, [x3, #8]
        assert_eq!((r.x[1], r.x[2]), (r.word(8, 4), r.word(12, 4)));
        r.mem[8..12].copy_from_slice(&0x8000_0000u32.to_le_bytes());
        r.run(0x6941_0861, 0); // ldpsw x1, x2, [x3, #8]
        assert_eq!(r.x[1], 0xffff_ffff_8000_0000);
        assert_eq!(r.x[2], r.word(12, 4) | if r.mem[15] & 0x80 != 0 { 0xffff_ffff_0000_0000 } else { 0 });
        r.x[1] = 11;
        r.x[2] = 22;
        r.run(0xa901_0861, 0); // stp x1, x2, [x3, #16]
        assert_eq!((r.word(16, 8), r.word(24, 8)), (11, 22));
        r.run(0xa900_fc7f, 0); // stp xzr, xzr, [x3, #8]
        assert_eq!((r.word(8, 8), r.word(16, 8)), (0, 0));
    }

    #[test]
    fn simd_registers_move_whole_and_zero_extend() {
        let mut r = Rig::new();
        r.run(0x3dc0_0841, 0); // ldr q1, [x2, #32]
        let mut q = [0u8; 16];
        q.copy_from_slice(&r.mem[32..48]);
        assert_eq!(r.v[1], u128::from_le_bytes(q));
        r.run(0xfd40_0841, 0); // ldr d1, [x2, #16]
        assert_eq!(r.v[1], u128::from(r.word(16, 8)));
        r.run(0xbd40_0c41, 0); // ldr s1, [x2, #12]
        assert_eq!(r.v[1], u128::from(r.word(12, 4)));
        r.run(0x3d40_0c41, 0); // ldr b1, [x2, #3]
        assert_eq!(r.v[1], u128::from(r.mem[3]));
        r.run(0x7d40_0c41, 0); // ldr h1, [x2, #6]
        assert_eq!(r.v[1], u128::from(r.word(6, 2)));
        r.v[1] = 0x0f0e_0d0c_0b0a_0908_0706_0504_0302_0100;
        r.run(0x3d80_0841, 0); // str q1, [x2, #32]
        assert_eq!(&r.mem[32..48], &r.v[1].to_le_bytes());
        r.run(0xfd00_0841, 0); // str d1, [x2, #16]
        assert_eq!(r.word(16, 8), 0x0706_0504_0302_0100);
        r.run(0xbd00_0c41, 0); // str s1, [x2, #12]
        assert_eq!(r.word(12, 4), 0x0302_0100);
        r.run(0x7d00_0c41, 0); // str h1, [x2, #6]
        assert_eq!(r.word(6, 2), 0x0100);
        r.run(0x3d00_0c41, 0); // str b1, [x2, #3]
        assert_eq!(r.mem[3], 0);
        r.v[2] = 0x1f1e_1d1c_1b1a_1918_1716_1514_1312_1110;
        r.run(0xad00_0861, 0); // stp q1, q2, [x3]
        assert_eq!(&r.mem[0..16], &r.v[1].to_le_bytes());
        assert_eq!(&r.mem[16..32], &r.v[2].to_le_bytes());
        r.run(0xad41_0861, 0); // ldp q1, q2, [x3, #32]
        assert_eq!(r.v[1], u128::from_le_bytes(r.mem[32..48].try_into().unwrap()));
        assert_eq!(r.v[2], u128::from_le_bytes(r.mem[48..64].try_into().unwrap()));
        r.run(0x6d41_0861, 0); // ldp d1, d2, [x3, #16]
        assert_eq!((r.v[1], r.v[2]), (u128::from(r.word(16, 8)), u128::from(r.word(24, 8))));
    }

    #[test]
    fn misaligned_accesses_copy_bytes() {
        let mut r = Rig::new();
        r.run(0xf940_0041, 1); // ldr x1, [x2] from an odd address
        assert_eq!(r.x[1], r.word(1, 8));
        r.x[1] = 0x1122_3344_5566_7788;
        r.run(0xf900_0041, 3); // str x1, [x2] to an odd address
        assert_eq!(r.word(3, 8), 0x1122_3344_5566_7788);
    }

    #[test]
    fn ordered_loads_and_stores_use_the_register_as_the_address() {
        let mut r = Rig::new();
        r.run(0xc8df_fc41, 8); // ldar x1, [x2]
        assert_eq!(r.x[1], r.word(8, 8));
        r.run(0x08df_fc41, 9); // ldarb w1, [x2]
        assert_eq!(r.x[1], r.word(9, 1));
        r.x[1] = 0xdead_beef;
        r.run(0x889f_fc41, 16); // stlr w1, [x2]
        assert_eq!(r.word(16, 4), 0xdead_beef);
        r.run(0xf8bf_c041, 24); // ldapr x1, [x2]
        assert_eq!(r.x[1], r.word(24, 8));
    }
}
