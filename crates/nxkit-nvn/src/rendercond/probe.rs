//! `NXKIT_DIAG=rendercond`, one line per distinct condition address (the first 256): what the condition read of its
//! two counters (the table entry, what landed at the address and at the place of the report) and the landed
//! addresses nearest to it, with the addresses of the reports of the frame so far. It tells whether a condition reads
//! where the reports land, and whether the game moves its reports to other addresses every frame.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::OnceLock;

use super::{Read, RenderConds, Report};

/// The condition addresses that get a line.
const MAX_ADDRS: usize = 256;

/// The state of the one-shot lines.
#[derive(Debug, Default)]
pub(super) struct Probe {
    seen: HashSet<u64>,
    /// The lowest and highest address of the emulated reports of the frame so far, and their number.
    reports: Option<(u64, u64)>,
    n: u32,
}

fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| crate::diag::on("rendercond"))
}

impl Probe {
    /// An emulated report at `addr` was recorded.
    pub(super) fn reported(&mut self, addr: u64) {
        if !on() {
            return;
        }
        self.n += 1;
        self.reports = Some(match self.reports {
            Some((lo, hi)) => (lo.min(addr), hi.max(addr)),
            None => (addr, addr),
        });
    }

    /// The frame was flushed.
    pub(super) fn flushed(&mut self) {
        (self.reports, self.n) = (None, 0);
    }
}

impl RenderConds {
    /// Prints the line of a condition at `addr`, once per address.
    pub(super) fn probe(&mut self, mode: u32, addr: u64, a: Read, b: Read, r: Option<bool>) {
        if !on() || self.diag.seen.len() >= MAX_ADDRS || !self.diag.seen.insert(addr) {
            return;
        }
        let mut line = format!(
            "DIAG rendercond cond at {addr:#x} mode {mode} -> {r:?}: [{}] [{}]",
            self.describe(addr, a),
            self.describe(addr.wrapping_add(16), b)
        );
        let landed = self.counters.landed();
        let [below, above] = landed.nearest(addr);
        let _ = write!(
            line,
            " | nearest landed (addr, value, seq) below {:x?} above {:x?} (of {})",
            below,
            above,
            landed.len()
        );
        let _ = write!(
            line,
            " | reports of the frame so far {} at {:x?}, newest seq {}",
            self.diag.n,
            self.diag.reports,
            self.counters.newest_seq()
        );
        eprintln!("{line}");
    }

    fn describe(&self, addr: u64, read: Read) -> String {
        let landed = self.counters.landed();
        let entry = match self.reports.get(&addr) {
            Some(Report::Known { value, flush }) => {
                format!("known {value:#x} {} flushes old", self.flush - flush)
            }
            Some(Report::Gpu { seq, place }) => format!(
                "gpu seq {seq} place {place:?}, landed at place {:?}",
                landed.at_place(*place)
            ),
            Some(Report::Unknown) => "gpu-only".to_string(),
            None => "not in the table, guest memory".to_string(),
        };
        format!(
            "{addr:#x}: {entry}, landed at addr {:?}, read {read:?}",
            landed.get(addr)
        )
    }
}
