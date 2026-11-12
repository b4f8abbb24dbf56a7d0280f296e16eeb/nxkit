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
//! [`decode_op`] adds the atomics of the large system extensions, `CAS`/`CASP`, the exclusives and `LD1`/`ST1` of one to
//! four registers. Anything else gets a [`Reason`] (the other structure loads, `DC ZVA`,
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
    /// `LDADD`, `LDCLR`, `LDEOR`, `LDSET`, `LDSMAX`, `LDSMIN`, `LDUMAX`, `LDUMIN` and `SWP`.
    Rmw = 12,
    /// `CAS`, `CASP`.
    Cas = 13,
    /// `LDXR`, `LDAXR`, `LDXP`, `LDAXP`.
    Ldxr = 14,
    /// `STXR`, `STLXR`, `STXP`, `STLXP`.
    Stxr = 15,
    /// `LD1` of one to four registers.
    Ld1 = 16,
    /// `ST1` of one to four registers.
    St1 = 17,
}

impl Form {
    /// The number of forms.
    pub const COUNT: usize = 18;

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
        Form::Rmw,
        Form::Cas,
        Form::Ldxr,
        Form::Stxr,
        Form::Ld1,
        Form::St1,
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
            Form::Rmw => "atomic",
            Form::Cas => "cas",
            Form::Ldxr => "ldxr",
            Form::Stxr => "stxr",
            Form::Ld1 => "ld1",
            Form::St1 => "st1",
        }
    }

    /// True for the loads.
    pub fn is_load(self) -> bool {
        (self as usize) < Form::Str as usize || matches!(self, Form::Ldxr | Form::Ld1)
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

// ---------------------------------------------------------------------------------------------------------------------
// Atomics, exclusives and LD1/ST1.

/// The operation of an atomic memory instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RmwOp {
    /// `LDADD`.
    Add,
    /// `LDCLR`: and with the complement.
    Clr,
    /// `LDEOR`.
    Eor,
    /// `LDSET`: or.
    Set,
    /// `LDSMAX`.
    Smax,
    /// `LDSMIN`.
    Smin,
    /// `LDUMAX`.
    Umax,
    /// `LDUMIN`.
    Umin,
    /// `SWP`.
    Swp,
}

/// An instruction of the large system extensions, an exclusive, or `LD1`/`ST1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    /// A plain load or store.
    Mem(Mem),
    /// `LDADD` and the others: `Rt` gets the old value, the register `rs` is the operand.
    Rmw {
        /// The operation.
        op: RmwOp,
        /// Bytes: 1, 2, 4 or 8.
        size: u8,
        /// The operand register.
        rs: u8,
        /// The result register (31 discards).
        rt: u8,
        /// The base register.
        rn: u8,
        /// Acquire and release bits.
        acq: bool,
        /// Release.
        rel: bool,
    },
    /// `CAS` and `CASP`: compare `rs` (and `rs + 1`) with memory, store `rt` (and `rt + 1`), `rs` gets the old value.
    Cas {
        /// Bytes per register: 1, 2, 4 or 8.
        size: u8,
        /// A pair.
        pair: bool,
        /// The compare register.
        rs: u8,
        /// The new value register.
        rt: u8,
        /// The base register.
        rn: u8,
        /// Acquire.
        acq: bool,
        /// Release.
        rel: bool,
    },
    /// `LDXR`, `LDXP`, `STXR`, `STXP`.
    Excl {
        /// A load.
        load: bool,
        /// A pair.
        pair: bool,
        /// Bytes per register: 4 or 8 for a pair, 1 to 8 else.
        size: u8,
        /// The status register of a store.
        rs: u8,
        /// The (first) data register.
        rt: u8,
        /// The second data register.
        rt2: u8,
        /// The base register.
        rn: u8,
        /// Acquire (loads) or release (stores).
        ord: bool,
    },
    /// `LD1`/`ST1` of `regs` registers of 8 or 16 bytes, optionally with a post index (a register, 31 for the size).
    Ld1 {
        /// A load.
        load: bool,
        /// 16 bytes per register, else 8.
        q: bool,
        /// One to four registers.
        regs: u8,
        /// The first register.
        rt: u8,
        /// The base register.
        rn: u8,
        /// The post-index register: None for no writeback, 31 for the immediate.
        post: Option<u8>,
    },
    /// `LD1`/`ST1` of one lane of a register (single structure), optionally with a post index.
    Lane {
        /// A load.
        load: bool,
        /// Bytes of the lane: 1, 2, 4 or 8.
        size: u8,
        /// The lane number.
        index: u8,
        /// The register.
        rt: u8,
        /// The base register.
        rn: u8,
        /// The post-index register: None for no writeback, 31 for the immediate (the size of the lane).
        post: Option<u8>,
    },
}

impl Op {
    /// The form, for the counters.
    pub(crate) fn form(&self) -> Form {
        match self {
            Op::Mem(m) => m.form,
            Op::Rmw { .. } => Form::Rmw,
            Op::Cas { .. } => Form::Cas,
            Op::Excl { load: true, .. } => Form::Ldxr,
            Op::Excl { .. } => Form::Stxr,
            Op::Ld1 { load: true, .. } | Op::Lane { load: true, .. } => Form::Ld1,
            Op::Ld1 { .. } | Op::Lane { .. } => Form::St1,
        }
    }

    /// True when the instruction may write memory.
    pub(crate) fn writes(&self) -> bool {
        !self.form().is_load()
    }
}

/// Decodes the instruction word `i` into any operation the emulator knows.
pub(crate) fn decode_op(i: u32) -> Result<Op, Reason> {
    match decode(i) {
        Ok(m) => Ok(Op::Mem(m)),
        Err(r @ (Reason::Atomic | Reason::Exclusive | Reason::Structure)) => decode_special(i).ok_or(r),
        Err(r) => Err(r),
    }
}

fn decode_special(i: u32) -> Option<Op> {
    let rt = (i & 31) as u8;
    let rn = ((i >> 5) & 31) as u8;
    let rs = ((i >> 16) & 31) as u8;
    let size = ((i >> 30) & 3) as u8;
    // Atomic memory operations: `size 111 0 00 A R 1 Rs o3 opc 00 Rn Rt`.
    if i & 0x3f20_0c00 == 0x3820_0000 {
        let (acq, rel) = (i >> 23 & 1 == 1, i >> 22 & 1 == 1);
        let o3 = i >> 15 & 1;
        let op = match (o3, (i >> 12) & 7) {
            (0, 0) => RmwOp::Add,
            (0, 1) => RmwOp::Clr,
            (0, 2) => RmwOp::Eor,
            (0, 3) => RmwOp::Set,
            (0, 4) => RmwOp::Smax,
            (0, 5) => RmwOp::Smin,
            (0, 6) => RmwOp::Umax,
            (0, 7) => RmwOp::Umin,
            (1, 0) => RmwOp::Swp,
            _ => return None,
        };
        return Some(Op::Rmw { op, size: 1 << size, rs, rt, rn, acq, rel });
    }
    // `CAS`: `size 001000 1 L 1 Rs o0 11111 Rn Rt`.
    if i & 0x3fa0_7c00 == 0x08a0_7c00 {
        return Some(Op::Cas { size: 1 << size, pair: false, rs, rt, rn, acq: i >> 22 & 1 == 1, rel: i >> 15 & 1 == 1 });
    }
    // `CASP`: `0 sz 001000 0 L 1 Rs o0 11111 Rn Rt`.
    if i & 0xbfa0_7c00 == 0x0820_7c00 {
        // Even registers only.
        if !rs.is_multiple_of(2) || !rt.is_multiple_of(2) {
            return None;
        }
        return Some(Op::Cas { size: 4 << size, pair: true, rs, rt, rn, acq: i >> 22 & 1 == 1, rel: i >> 15 & 1 == 1 });
    }
    // `LDXR`, `STXR`: `size 001000 0 L 0 Rs o0 Rt2 Rn Rt`.
    if i & 0x3fa0_0000 == 0x0800_0000 {
        let load = i >> 22 & 1 == 1;
        if load && (rs != 31 || (i >> 10) & 31 != 31) {
            return None;
        }
        return Some(Op::Excl { load, pair: false, size: 1 << size, rs, rt, rt2: 31, rn, ord: i >> 15 & 1 == 1 });
    }
    // `LDXP`, `STXP`: sizes `10` and `11`.
    if i & 0x3fa0_0000 == 0x0820_0000 && size >= 2 {
        let load = i >> 22 & 1 == 1;
        return Some(Op::Excl {
            load,
            pair: true,
            size: 1 << size,
            rs,
            rt,
            rt2: ((i >> 10) & 31) as u8,
            rn,
            ord: i >> 15 & 1 == 1,
        });
    }
    // `LD1`, `ST1` (multiple structures, one to four registers): `0 Q 0011000 L 000000 opcode size Rn Rt`, or with a post
    // index `0 Q 0011001 L 0 Rm opcode size Rn Rt`.
    let plain = i & 0xbfbf_0000 == 0x0c00_0000;
    let postx = i & 0xbfa0_0000 == 0x0c80_0000;
    if plain || postx {
        let regs = match (i >> 12) & 15 {
            0b0111 => 1,
            0b1010 => 2,
            0b0110 => 3,
            0b0010 => 4,
            _ => return None,
        };
        return Some(Op::Ld1 { load: i >> 22 & 1 == 1, q: i >> 30 & 1 == 1, regs, rt, rn, post: postx.then_some(rs) });
    }
    // `LD1`, `ST1` (one lane): `0 Q 0011010 L 0 00000 opcode S size Rn Rt`, or with a post index
    // `0 Q 0011011 L 0 Rm opcode S size Rn Rt`.
    let lane_plain = i & 0xbfbf_0000 == 0x0d00_0000;
    let lane_post = i & 0xbfa0_0000 == 0x0d80_0000;
    if lane_plain || lane_post {
        let (q, s_bit, sz) = (i >> 30 & 1, i >> 12 & 1, i >> 10 & 3);
        let (size, index) = match (i >> 13) & 7 {
            0b000 => (1, q << 3 | s_bit << 2 | sz),
            0b010 if sz & 1 == 0 => (2, q << 2 | s_bit << 1 | sz >> 1),
            0b100 if sz == 0 => (4, q << 1 | s_bit),
            0b100 if sz == 1 && s_bit == 0 => (8, q),
            _ => return None,
        };
        return Some(Op::Lane {
            load: i >> 22 & 1 == 1,
            size,
            index: index as u8,
            rt,
            rn,
            post: lane_post.then_some(rs),
        });
    }
    None
}

/// The address of `op`, with what it writes back.
///
/// # Safety
/// `r` must be a valid register file.
pub(crate) unsafe fn plan_op(op: &Op, r: &Regs) -> Plan {
    // SAFETY: the caller guarantees the register file.
    unsafe {
        match op {
            Op::Mem(m) => plan(m, r),
            Op::Rmw { size, rn, .. } => Plan { ea: r.base(*rn), len: u64::from(*size), writeback: None },
            Op::Cas { size, pair, rn, .. } => {
                Plan { ea: r.base(*rn), len: u64::from(*size) * if *pair { 2 } else { 1 }, writeback: None }
            }
            Op::Excl { size, pair, rn, .. } => {
                Plan { ea: r.base(*rn), len: u64::from(*size) * if *pair { 2 } else { 1 }, writeback: None }
            }
            Op::Ld1 { q, regs, rn, post, .. } => {
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
    if matches!(o, Ordering::Acquire | Ordering::AcqRel) { Ordering::Acquire } else { Ordering::Relaxed }
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
    let mask = |size: u8, v: u64| if size >= 8 { v } else { v & ((1u64 << (u32::from(size) * 8)) - 1) };
    match op {
        Op::Mem(m) => {
            // SAFETY: per the caller.
            unsafe { perform(m, r, plan, p) };
            true
        }
        Op::Rmw { op, size, rs, rt, acq, rel, .. } => {
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
        Op::Cas { size, pair, rs, rt, acq, rel, .. } => {
            let total = usize::from(*size) * if *pair { 2 } else { 1 };
            if !(p as usize).is_multiple_of(total) {
                return false;
            }
            let o = ords(*acq, *rel);
            // SAFETY: per the caller.
            unsafe {
                if !*pair {
                    let old = cas(p, *size, mask(*size, r.gpr(*rs)), mask(*size, r.gpr(*rt)), o);
                    set(*rs, old);
                } else if *size == 4 {
                    let e = mask(4, r.gpr(*rs)) | mask(4, r.gpr(*rs + 1)) << 32;
                    let n = mask(4, r.gpr(*rt)) | mask(4, r.gpr(*rt + 1)) << 32;
                    let old = cas(p, 8, e, n, o);
                    set(*rs, old & 0xffff_ffff);
                    set(*rs + 1, old >> 32);
                } else {
                    let old = cas16(p, (r.gpr(*rs), r.gpr(*rs + 1)), (r.gpr(*rt), r.gpr(*rt + 1)));
                    set(*rs, old.0);
                    set(*rs + 1, old.1);
                }
            }
            true
        }
        Op::Excl { load, pair, size, rs, rt, rt2, ord, .. } => {
            let total = u64::from(*size) * if *pair { 2 } else { 1 };
            if !(p as usize).is_multiple_of(total as usize) {
                return false;
            }
            let ea = plan.ea;
            if *load {
                // SAFETY: per the caller.
                let (lo, hi) = unsafe {
                    let o = if *ord { Ordering::Acquire } else { Ordering::Relaxed };
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
                let o = if *ord { Ordering::Release } else { Ordering::Relaxed };
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
        Op::Lane { load, size, index, rt, rn, post } => {
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
        Op::Ld1 { load, q, regs, rt, .. } => {
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

    fn op(i: u32) -> Op {
        decode_op(i).unwrap_or_else(|e| panic!("{i:#010x} not decoded: {e:?}"))
    }

    #[test]
    fn atomic_memory_operations_decode_by_operation_size_and_order() {
        let rmw_of = |i: u32| match op(i) {
            Op::Rmw { op, size, rs, rt, rn, acq, rel } => (op, size, rs, rt, rn, acq, rel),
            o => panic!("{i:#010x}: {o:?}"),
        };
        assert_eq!(rmw_of(0xf821_0062), (RmwOp::Add, 8, 1, 2, 3, false, false));
        assert_eq!(rmw_of(0xb8a1_0062), (RmwOp::Add, 4, 1, 2, 3, true, false));
        assert_eq!(rmw_of(0xf8e1_0062), (RmwOp::Add, 8, 1, 2, 3, true, true));
        assert_eq!(rmw_of(0x3861_0062), (RmwOp::Add, 1, 1, 2, 3, false, true));
        assert_eq!(rmw_of(0x7821_0062), (RmwOp::Add, 2, 1, 2, 3, false, false));
        assert_eq!(rmw_of(0xf821_1062).0, RmwOp::Clr);
        assert_eq!(rmw_of(0xf821_2062).0, RmwOp::Eor);
        assert_eq!(rmw_of(0xf821_3062).0, RmwOp::Set);
        assert_eq!(rmw_of(0xf821_4062).0, RmwOp::Smax);
        assert_eq!(rmw_of(0xb821_5062).0, RmwOp::Smin);
        assert_eq!(rmw_of(0xf821_6062).0, RmwOp::Umax);
        assert_eq!(rmw_of(0xf821_7062).0, RmwOp::Umin);
        assert_eq!(rmw_of(0xf821_8062).0, RmwOp::Swp);
        assert_eq!(rmw_of(0xb8e1_8062), (RmwOp::Swp, 4, 1, 2, 3, true, true));
        assert_eq!(rmw_of(0xf821_007f).3, 31, "STADD discards the result");
    }

    #[test]
    fn compare_and_swap_exclusives_and_ld1_decode() {
        assert_eq!(
            op(0xc8a1_7c62),
            Op::Cas { size: 8, pair: false, rs: 1, rt: 2, rn: 3, acq: false, rel: false }
        );
        assert_eq!(
            op(0x88e1_fc62),
            Op::Cas { size: 4, pair: false, rs: 1, rt: 2, rn: 3, acq: true, rel: true }
        );
        assert_eq!(op(0x08a1_7c62), Op::Cas { size: 1, pair: false, rs: 1, rt: 2, rn: 3, acq: false, rel: false });
        assert_eq!(op(0x4822_7cc4), Op::Cas { size: 8, pair: true, rs: 2, rt: 4, rn: 6, acq: false, rel: false });
        assert_eq!(op(0x0862_fcc4), Op::Cas { size: 4, pair: true, rs: 2, rt: 4, rn: 6, acq: true, rel: true });
        let ex = |i| match op(i) {
            Op::Excl { load, pair, size, rs, rt, rt2, rn, ord } => (load, pair, size, rs, rt, rt2, rn, ord),
            o => panic!("{i:#010x}: {o:?}"),
        };
        assert_eq!(ex(0xc85f_7c41), (true, false, 8, 31, 1, 31, 2, false));
        assert_eq!(ex(0x885f_fc41), (true, false, 4, 31, 1, 31, 2, true));
        assert_eq!(ex(0x085f_7c41), (true, false, 1, 31, 1, 31, 2, false));
        assert_eq!(ex(0xc803_7c41), (false, false, 8, 3, 1, 31, 2, false));
        assert_eq!(ex(0x8803_fc41), (false, false, 4, 3, 1, 31, 2, true));
        assert_eq!(ex(0x4803_7c41), (false, false, 2, 3, 1, 31, 2, false));
        assert_eq!(ex(0xc87f_0861), (true, true, 8, 31, 1, 2, 3, false));
        assert_eq!(ex(0x887f_8861), (true, true, 4, 31, 1, 2, 3, true));
        assert_eq!(ex(0xc824_0861), (false, true, 8, 4, 1, 2, 3, false));
        assert_eq!(ex(0x8824_8861), (false, true, 4, 4, 1, 2, 3, true));
        let ld1 = |i| match op(i) {
            Op::Ld1 { load, q, regs, rt, rn, post } => (load, q, regs, rt, rn, post),
            o => panic!("{i:#010x}: {o:?}"),
        };
        assert_eq!(ld1(0x4c40_7020), (true, true, 1, 0, 1, None));
        assert_eq!(ld1(0x0c40_7020), (true, false, 1, 0, 1, None));
        assert_eq!(ld1(0x4c40_a820), (true, true, 2, 0, 1, None));
        assert_eq!(ld1(0x4c40_6c20), (true, true, 3, 0, 1, None));
        assert_eq!(ld1(0x4c40_203e), (true, true, 4, 30, 1, None));
        assert_eq!(ld1(0x4c00_7020), (false, true, 1, 0, 1, None));
        assert_eq!(ld1(0x0c9f_a020), (false, false, 2, 0, 1, Some(31)));
        assert_eq!(ld1(0x4c82_7020), (false, true, 1, 0, 1, Some(2)));
        assert_eq!(ld1(0x4cdf_a020), (true, true, 2, 0, 1, Some(31)));
        // What stays out: LD2, LD1R, the lane forms of two registers.
        assert_eq!(decode_op(0x4c40_8820).err(), Some(Reason::Structure));
        assert_eq!(decode_op(0x0d40_c820).err(), Some(Reason::Structure));
        assert_eq!(decode_op(0x0d60_9020).err(), Some(Reason::Structure));
        // One lane: `ST1 {v0.s}[1], [x22]`, `ST1 {v0.s}[2], [x8]`, `LD1 {v0.s}[1], [x1]`, `LD1 {v3.b}[15], [x2], #1`,
        // `ST1 {v1.d}[1], [x2], x3`, `ST1 {v1.h}[5], [x2]`.
        let lane = |i| match op(i) {
            Op::Lane { load, size, index, rt, rn, post } => (load, size, index, rt, rn, post),
            o => panic!("{i:#010x}: {o:?}"),
        };
        assert_eq!(lane(0x0d00_92c0), (false, 4, 1, 0, 22, None));
        assert_eq!(lane(0x4d00_8100), (false, 4, 2, 0, 8, None));
        assert_eq!(lane(0x0d40_9020), (true, 4, 1, 0, 1, None));
        assert_eq!(lane(0x4ddf_1c43), (true, 1, 15, 3, 2, Some(31)));
        assert_eq!(lane(0x4d83_8441), (false, 8, 1, 1, 2, Some(3)));
        assert_eq!(lane(0x4d00_4841), (false, 2, 5, 1, 2, None));
    }

    impl Rig {
        fn run_op(&mut self, i: u32, at: usize) -> bool {
            let o = op(i);
            let rn = match o {
                Op::Rmw { rn, .. }
                | Op::Cas { rn, .. }
                | Op::Excl { rn, .. }
                | Op::Ld1 { rn, .. }
                | Op::Lane { rn, .. } => rn,
                Op::Mem(m) => m.rn,
            };
            let base = self.mem.as_mut_ptr() as u64 + at as u64;
            if rn == 31 {
                self.sp = base;
            } else {
                self.x[usize::from(rn)] = base;
            }
            let r = self.regs();
            // SAFETY: the rig's own register file and buffer.
            unsafe {
                let p = plan_op(&o, &r);
                let off = p.ea.wrapping_sub(self.mem.as_ptr() as u64) as usize;
                perform_op(&o, &r, &p, self.mem.as_mut_ptr().add(off))
            }
        }
    }

    #[test]
    fn atomic_operations_give_the_old_value_and_store_the_result() {
        let mut r = Rig::new();
        // Aligned positions: the rig buffer has no alignment promise, so find one.
        let a = (8 - (r.mem.as_ptr() as usize) % 8) % 8 + 16;
        let put = |r: &mut Rig, v: u64| r.mem[a..a + 8].copy_from_slice(&v.to_le_bytes());
        let get = |r: &Rig| u64::from_le_bytes(r.mem[a..a + 8].try_into().unwrap());
        for (i, f, start, operand) in [
            (0xf821_0062u32, (|m: u64, v: u64| m.wrapping_add(v)) as fn(u64, u64) -> u64, 5u64, 7u64),
            (0xf821_1062, |m, v| m & !v, 0xff, 0x0f),
            (0xf821_2062, |m, v| m ^ v, 0xf0, 0xff),
            (0xf821_3062, |m, v| m | v, 0xf0, 0x0f),
            (0xf821_4062, |m, v| (m as i64).max(v as i64) as u64, (-5i64) as u64, 3),
            (0xf821_5062, |m, v| (m as i64).min(v as i64) as u64, (-5i64) as u64, 3),
            (0xf821_6062, |m, v| m.max(v), (-5i64) as u64, 3),
            (0xf821_7062, |m, v| m.min(v), (-5i64) as u64, 3),
            (0xf821_8062, |_, v| v, 9, 3),
        ] {
            put(&mut r, start);
            r.x[1] = operand;
            assert!(r.run_op(i, a));
            assert_eq!(r.x[2], start, "{i:#010x} old value");
            assert_eq!(get(&r), f(start, operand), "{i:#010x} result");
        }
        // A word add wraps at 32 bits and leaves the next bytes alone.
        r.mem[a..a + 8].copy_from_slice(&[0xff, 0xff, 0xff, 0xff, 7, 7, 7, 7]);
        r.x[1] = 2;
        assert!(r.run_op(0xb821_0062, a)); // ldadd w1, w2, [x3]
        assert_eq!(&r.mem[a..a + 8], &[1, 0, 0, 0, 7, 7, 7, 7]);
        assert_eq!(r.x[2], 0xffff_ffff);
        // A misaligned atomic is refused and changes nothing.
        let before = r.mem;
        assert!(!r.run_op(0xf821_0062, a + 1));
        assert_eq!(before, r.mem);
    }

    #[test]
    fn one_lane_stores_and_loads_touch_only_that_lane_and_write_back() {
        let mut r = Rig::new();
        r.v[0] = 0x1111_1111_2222_2222_3333_3333_4444_4444;
        // `ST1 {v0.s}[1], [x22]` stores the second word.
        let next = r.mem[12];
        assert!(r.run_op(0x0d00_92c0, 8));
        assert_eq!(&r.mem[8..12], &[0x33, 0x33, 0x33, 0x33]);
        assert_eq!(r.mem[12], next, "only four bytes");
        // `LD1 {v0.s}[2], [x1]` (0x4d40_8020) replaces the third word and keeps the others.
        r.mem[20..24].copy_from_slice(&[9, 8, 7, 6]);
        assert!(r.run_op(0x4d40_8020, 20));
        assert_eq!(r.v[0], 0x1111_1111_0607_0809_3333_3333_4444_4444u128, "lane 2 is the word at bits 64 to 96");
    }

    #[test]
    fn compare_and_swap_and_pairs() {
        let mut r = Rig::new();
        let a = (16 - (r.mem.as_ptr() as usize) % 16) % 16 + 16;
        r.mem[a..a + 8].copy_from_slice(&5u64.to_le_bytes());
        r.x[1] = 5; // expected
        r.x[2] = 9; // new
        assert!(r.run_op(0xc8a1_7c62, a)); // cas x1, x2, [x3]
        assert_eq!(u64::from_le_bytes(r.mem[a..a + 8].try_into().unwrap()), 9);
        assert_eq!(r.x[1], 5);
        r.x[1] = 5;
        r.x[2] = 11;
        assert!(r.run_op(0xc8a1_7c62, a)); // fails now: memory holds 9
        assert_eq!(u64::from_le_bytes(r.mem[a..a + 8].try_into().unwrap()), 9);
        assert_eq!(r.x[1], 9, "the old value comes back");
        // CASP of two x registers: compare x2, x3 with the 16 bytes, store x4, x5.
        r.mem[a..a + 8].copy_from_slice(&1u64.to_le_bytes());
        r.mem[a + 8..a + 16].copy_from_slice(&2u64.to_le_bytes());
        (r.x[2], r.x[3], r.x[4], r.x[5]) = (1, 2, 30, 40);
        assert!(r.run_op(0x4822_7cc4, a)); // casp x2, x3, x4, x5, [x6]
        assert_eq!(u64::from_le_bytes(r.mem[a..a + 8].try_into().unwrap()), 30);
        assert_eq!(u64::from_le_bytes(r.mem[a + 8..a + 16].try_into().unwrap()), 40);
        // CASP of two w registers.
        r.mem[a..a + 4].copy_from_slice(&7u32.to_le_bytes());
        r.mem[a + 4..a + 8].copy_from_slice(&8u32.to_le_bytes());
        (r.x[2], r.x[3], r.x[4], r.x[5]) = (7, 8, 70, 80);
        assert!(r.run_op(0x0862_fcc4, a)); // caspal w2, w3, w4, w5, [x6]
        assert_eq!(&r.mem[a..a + 8], &[70, 0, 0, 0, 80, 0, 0, 0]);
    }

    #[test]
    fn a_store_exclusive_succeeds_only_after_its_load_exclusive_of_the_same_address() {
        let mut r = Rig::new();
        let a = (16 - (r.mem.as_ptr() as usize) % 16) % 16 + 16;
        r.mem[a..a + 8].copy_from_slice(&5u64.to_le_bytes());
        // stxr without a load: fails.
        r.x[1] = 99;
        assert!(r.run_op(0xc803_7c41, a)); // stxr w3, x1, [x2]
        assert_eq!(r.x[3], 1);
        assert_eq!(u64::from_le_bytes(r.mem[a..a + 8].try_into().unwrap()), 5);
        // ldxr then stxr: succeeds.
        assert!(r.run_op(0xc85f_7c41, a)); // ldxr x1, [x2]
        assert_eq!(r.x[1], 5);
        r.x[1] = 6;
        assert!(r.run_op(0xc803_7c41, a));
        assert_eq!(r.x[3], 0);
        assert_eq!(u64::from_le_bytes(r.mem[a..a + 8].try_into().unwrap()), 6);
        // The monitor is gone after the store: a second store fails.
        assert!(r.run_op(0xc803_7c41, a));
        assert_eq!(r.x[3], 1);
        // Memory changed in between: the store fails.
        assert!(r.run_op(0xc85f_7c41, a));
        r.mem[a] = 77;
        r.x[1] = 1;
        assert!(r.run_op(0xc803_7c41, a));
        assert_eq!(r.x[3], 1);
        // A load of another address does not satisfy a store to this one.
        assert!(r.run_op(0xc85f_7c41, a + 8));
        assert!(r.run_op(0xc803_7c41, a));
        assert_eq!(r.x[3], 1);
        // Pairs.
        r.mem[a..a + 8].copy_from_slice(&1u64.to_le_bytes());
        r.mem[a + 8..a + 16].copy_from_slice(&2u64.to_le_bytes());
        assert!(r.run_op(0xc87f_0861, a)); // ldxp x1, x2, [x3]
        assert_eq!((r.x[1], r.x[2]), (1, 2));
        (r.x[1], r.x[2]) = (10, 20);
        assert!(r.run_op(0xc824_0861, a)); // stxp w4, x1, x2, [x3]
        assert_eq!(r.x[4], 0);
        assert_eq!(u64::from_le_bytes(r.mem[a..a + 8].try_into().unwrap()), 10);
        assert_eq!(u64::from_le_bytes(r.mem[a + 8..a + 16].try_into().unwrap()), 20);
    }

    #[test]
    fn ld1_and_st1_move_whole_registers_in_a_row() {
        let mut r = Rig::new();
        assert!(r.run_op(0x4c40_a820, 0)); // ld1 {v0.4s, v1.4s}, [x1]
        assert_eq!(r.v[0], u128::from_le_bytes(r.mem[0..16].try_into().unwrap()));
        assert_eq!(r.v[1], u128::from_le_bytes(r.mem[16..32].try_into().unwrap()));
        assert!(r.run_op(0x0c40_7020, 40)); // ld1 {v0.8b}, [x1]: the upper half clears
        assert_eq!(r.v[0], u128::from(r.word(40, 8)));
        // ld1 of four registers wrapping from v30 to v1.
        assert!(r.run_op(0x4c40_203e, 0));
        assert_eq!(r.v[30], u128::from_le_bytes(r.mem[0..16].try_into().unwrap()));
        assert_eq!(r.v[1], u128::from_le_bytes(r.mem[48..64].try_into().unwrap()));
        // st1 with an immediate post index writes the base back.
        r.v[0] = 0x0f0e_0d0c_0b0a_0908_0706_0504_0302_0100;
        r.v[1] = 0x1f1e_1d1c_1b1a_1918_1716_1514_1312_1110;
        assert!(r.run_op(0x0c9f_a020, 64)); // st1 {v0.8b, v1.8b}, [x1], #16
        assert_eq!(r.word(64, 8), 0x0706_0504_0302_0100);
        assert_eq!(r.word(72, 8), 0x1716_1514_1312_1110);
        assert_eq!(r.x[1], r.mem.as_ptr() as u64 + 64 + 16);
        // A register post index.
        r.x[2] = 24;
        assert!(r.run_op(0x4c82_7020, 80)); // st1 {v0.16b}, [x1], x2
        assert_eq!(r.x[1], r.mem.as_ptr() as u64 + 80 + 24);
    }
}
