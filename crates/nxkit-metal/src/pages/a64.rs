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

mod atomic;
mod decode;
mod exec;
#[cfg(test)]
mod tests;

pub(crate) use atomic::{perform_op, plan_op};
pub(crate) use decode::decode_op;
pub(crate) use exec::{Plan, Regs, perform, plan};

use std::sync::atomic::{AtomicU8, AtomicU16, AtomicU32, AtomicU64, Ordering};

use exec::read;

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
