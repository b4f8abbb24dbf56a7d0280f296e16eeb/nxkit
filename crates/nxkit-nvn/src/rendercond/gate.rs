//! What each render condition covered, for `NXKIT_DIAG=rendercond`. The front end records, for every run of commands
//! under a conditional render, the condition that decided it. The encoder prints what the commands are
//! (`nxkit-gpu`, `encode::gate`). Nothing is recorded unless the diagnostic is on.

use std::collections::HashMap;
use std::ops::Range;

use super::force::{Decision, Source};
use super::{Read, RenderConds};
use crate::frame::Frame;

/// One condition: what it read and what was decided.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Info {
    /// `NVNconditionalRenderMode` of the command.
    pub mode: u32,
    /// The GPU address of the first report.
    pub addr: u64,
    /// What the condition read at `addr` and at `addr + 16`.
    pub reads: [Read; 2],
    /// Where the operands came from.
    pub source: Source,
    /// The decision, with the switch that forced it.
    pub decision: Decision,
}

impl Info {
    /// The decision as a word: `render`, `skip`, or `forced-<knob>-<render|skip>`.
    pub fn decision_name(&self) -> String {
        let what = if self.decision.render {
            "render"
        } else {
            "skip"
        };
        match super::force::name(self.decision.forced) {
            "" => what.to_owned(),
            knob => format!("forced-{knob}-{what}"),
        }
    }
}

/// A run of commands of the frame that one condition covered.
#[derive(Clone, Debug, PartialEq)]
pub struct Seg {
    /// The commands.
    pub range: Range<u32>,
    /// The condition.
    pub info: Info,
}

/// The condition that each queue carries into its next submit, while the diagnostic is on.
#[derive(Debug, Default)]
pub(super) struct Gates {
    open: HashMap<usize, Info>,
}

/// Whether the segments are recorded.
pub fn enabled() -> bool {
    super::probe::on()
}

impl Gates {
    pub(super) fn get(&self, queue: usize) -> Option<Info> {
        self.open.get(&queue).copied()
    }

    pub(super) fn set(&mut self, queue: usize, info: Option<Info>) {
        match info {
            Some(i) => self.open.insert(queue, i),
            None => self.open.remove(&queue),
        };
    }
}

impl RenderConds {
    /// Adds the run `range` that `info` covers to the frame.
    pub(super) fn cover(frame: &mut Frame, range: Range<u32>, info: Option<Info>) {
        if let (Some(info), false) = (info, range.is_empty()) {
            frame.gates.push(Seg { range, info });
        }
    }
}
