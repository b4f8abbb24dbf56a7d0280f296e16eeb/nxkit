//! Run-time switches that change what a render condition decides. They are diagnostics for finding out what the
//! conditions of a game gate. All of them are off by default, and then every decision is the one that
//! [`super::RenderConds`] makes without them.
//!
//! `nxkit-gpu` owns the knobs and calls [`set_force`] when it reads them, the same way it hands the options of the pass
//! optimiser to `nxkit-nvn`. The guest thread reads the bits with one atomic load per condition.

use std::sync::atomic::{AtomicU8, Ordering};

use super::Read;

/// A condition whose two operands both came from guest memory renders (knob `rcflag`).
pub const RCFLAG: u8 = 1;
/// A condition with an unknown result is left out instead of rendered (knob `rcunk`).
pub const RCUNK: u8 = 2;
/// Every `SetRenderEnableConditional` renders (knob `rcall`). `SetRenderEnable` is not changed: the front end records
/// it as a plain on or off mark, which is not a condition on two reports.
pub const RCALL: u8 = 4;
/// No counter of an earlier report is read, like `NXKIT_RENDERCOND_STALE=0` (knob `rcnostale`).
pub const NOSTALE: u8 = 8;
/// A false condition leaves out draws and clears only, like `NXKIT_RENDERCOND=3d` (knob `rc3d`).
pub const ONLY_3D: u8 = 16;

static FORCE: AtomicU8 = AtomicU8::new(0);

/// Sets the switches: a mix of the constants of this module. 0 turns them all off.
pub fn set_force(bits: u8) {
    FORCE.store(bits, Ordering::Relaxed);
}

/// The switches that are on now.
pub fn force() -> u8 {
    FORCE.load(Ordering::Relaxed)
}

/// Where the operands of a condition came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// At least one operand is a report of the table: a value of a recording or a GPU counter.
    Counter,
    /// Both operands are plain guest memory.
    Memory,
    /// The result is unknown: an operand or the mode is not known.
    Unknown,
}

impl Source {
    /// The word of the diagnostic lines.
    pub fn name(self) -> &'static str {
        match self {
            Self::Counter => "counter",
            Self::Memory => "memory",
            Self::Unknown => "unknown",
        }
    }
}

/// The source of a condition with the natural result `natural` and the reads `a` and `b`.
pub(super) fn source(natural: Option<bool>, a: Read, b: Read) -> Source {
    match (natural, a, b) {
        (None, ..) => Source::Unknown,
        (_, Read::Memory(_), Read::Memory(_)) => Source::Memory,
        _ => Source::Counter,
    }
}

/// What the switches made of one decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decision {
    /// Whether the condition renders.
    pub render: bool,
    /// The switch that forced the decision (one of [`RCFLAG`], [`RCUNK`], [`RCALL`]), 0 when none did.
    pub forced: u8,
}

/// The decision for a condition: `natural` is the result of the comparison, None when it is unknown (renders).
/// `RCALL` wins over the other two.
pub(super) fn decide(natural: Option<bool>, source: Source, bits: u8) -> Decision {
    let by = if bits & RCALL != 0 {
        RCALL
    } else if bits & RCFLAG != 0 && source == Source::Memory {
        RCFLAG
    } else if bits & RCUNK != 0 && natural.is_none() {
        RCUNK
    } else {
        0
    };
    Decision {
        render: match by {
            RCALL | RCFLAG => true,
            RCUNK => false,
            _ => natural.unwrap_or(true),
        },
        forced: by,
    }
}

/// The name of a switch for the diagnostic lines.
pub fn name(bit: u8) -> &'static str {
    match bit {
        RCFLAG => "rcflag",
        RCUNK => "rcunk",
        RCALL => "rcall",
        _ => "",
    }
}
