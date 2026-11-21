//! The handlers of the observation gates (R46): one per id, registered by the composition root from the subsystems
//! that own them (`obswire`).
//!
//! An `obs` gate saves the guest's registers in the frame of the svc gate (`gate.rs`) and calls [`ObsTable::run`]
//! with its argument. The handler of the id gets x0 to x28 and the parameter of the argument; what it writes there
//! takes effect when the gate returns, as for a svc (x18 excepted). An id without a handler passes, and its first
//! pass is logged once.

use std::sync::atomic::{AtomicU64, Ordering};

use nxkit_abi::obs::{Handler, REGS, split};

/// The handlers by id.
pub struct ObsTable {
    handlers: Vec<Option<Handler>>,
    /// One bit per id that passed without a handler and was logged.
    unknown: [AtomicU64; 4],
}

impl Default for ObsTable {
    fn default() -> Self {
        ObsTable {
            handlers: (0..256).map(|_| None).collect(),
            unknown: Default::default(),
        }
    }
}

impl std::fmt::Debug for ObsTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ids: Vec<usize> = (0..256).filter(|&i| self.handlers[i].is_some()).collect();
        f.debug_struct("ObsTable").field("ids", &ids).finish()
    }
}

impl ObsTable {
    /// Sets the handler of `id`, in place of an earlier one.
    pub fn set(&mut self, id: u8, h: Handler) {
        self.handlers[usize::from(id)] = Some(h);
    }

    /// Whether `id` has a handler.
    pub fn has(&self, id: u8) -> bool {
        self.handlers[usize::from(id)].is_some()
    }

    /// Runs the handler of the gate with argument `arg` over the registers `regs` (x0 to x28, at least
    /// [`REGS`] words). Returns whether a handler ran.
    pub fn run(&self, arg: u32, regs: &mut [u64], ret: u64) -> bool {
        let (id, param) = split(arg);
        if regs.len() < REGS {
            return false;
        }
        match &self.handlers[usize::from(id)] {
            Some(h) => {
                h(&mut regs[..REGS], param);
                true
            }
            None => {
                let bit = 1u64 << (id % 64);
                let first =
                    self.unknown[usize::from(id / 64)].fetch_or(bit, Ordering::Relaxed) & bit == 0;
                if first {
                    eprintln!(
                        "nxkit-host: observation gate {id} (parameter {param}, first at {ret:#x}) has no handler and passes"
                    );
                }
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU32;

    #[test]
    fn a_handler_gets_its_parameter_and_changes_the_registers() {
        let mut t = ObsTable::default();
        t.set(26, Box::new(|r, p| r[p as usize] = 7));
        let mut regs = [1u64; REGS];
        assert!(t.run(26 | 5 << 8, &mut regs, 0));
        assert_eq!(regs[5], 7);
        assert_eq!(regs[4], 1);
        assert!(t.has(26) && !t.has(25));
    }

    #[test]
    fn an_unknown_id_passes_and_leaves_the_registers() {
        let calls = Arc::new(AtomicU32::new(0));
        let mut t = ObsTable::default();
        let c = calls.clone();
        t.set(
            21,
            Box::new(move |_, _| {
                c.fetch_add(1, Ordering::Relaxed);
            }),
        );
        let mut regs = [3u64; REGS];
        assert!(!t.run(25, &mut regs, 0x1000));
        assert!(!t.run(25, &mut regs, 0x1000));
        assert_eq!(regs, [3u64; REGS]);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert!(!t.run(21, &mut regs[..10], 0), "a short frame runs nothing");
    }
}
