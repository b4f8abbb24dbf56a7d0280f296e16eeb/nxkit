//! The core budget in P-core equivalents (design-v2 5 and 8.1, decisions.md W15).

/// How many tokens (concurrent units of CPU work, in-process or a child process) the executor hands out, and how many
/// worker threads it runs. Each running task holds `cost` tokens.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Budget {
    /// Tokens available at once.
    pub tokens: u32,
    /// Worker threads (at least `tokens`, so a token can wait on a child without idling the CPU budget).
    pub workers: usize,
    /// Bytes of expected peak memory that running tasks may hold together, 0 means no memory limit.
    pub mem_bytes: u64,
}

/// Default E-core throughput relative to a P core for these jobs (design-v2 5: estimate 0.35 to 0.5, P0 measures it).
pub const DEFAULT_E_FACTOR: f64 = 0.4;

impl Budget {
    /// A fixed number of tokens and the same number of workers (at least 1).
    pub fn fixed(tokens: u32) -> Budget {
        let tokens = tokens.max(1);
        Budget {
            tokens,
            workers: tokens as usize,
            mem_bytes: 0,
        }
    }

    /// The same budget with a memory limit in bytes (0 = none).
    pub fn with_memory(mut self, bytes: u64) -> Budget {
        self.mem_bytes = bytes;
        self
    }

    /// The default memory budget (design-v2 8.3): `0.5 × physical memory − 2 GiB`, at least 1 GiB.
    /// 0 (no limit) when the machine's memory is unknown. 32 GB → 14 GiB.
    pub fn default_memory() -> u64 {
        nxb_sys::system::memsize().map_or(0, memory_from_physical)
    }

    /// Tokens = round(P + e·E) + oversubscription, where e is the E-core throughput relative to a P core.
    ///
    /// ```
    /// use nxb_sched::Budget;
    /// // M2 Max: 8 P + 4 E, e = 0.4, oversubscription 2 -> 9.6 PCE -> 10 + 2 = 12 tokens.
    /// assert_eq!(Budget::from_cores(8, 4, 0.4, 2).tokens, 12);
    /// ```
    pub fn from_cores(p: u32, e: u32, e_factor: f64, oversubscribe: u32) -> Budget {
        let factor = if e_factor.is_finite() {
            e_factor.clamp(0.0, 1.0)
        } else {
            DEFAULT_E_FACTOR
        };
        let pce = f64::from(p) + f64::from(e) * factor;
        // pce is small and non-negative, the cast cannot overflow.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let base = pce.round() as u32;
        Budget::fixed(base.saturating_add(oversubscribe))
    }

    /// From this machine's core counts (`nxb-sys`).
    pub fn detect(e_factor: f64, oversubscribe: u32) -> Budget {
        let c = nxb_sys::system::core_counts();
        Budget::from_cores(c.performance, c.efficiency, e_factor, oversubscribe)
    }

    /// P-core equivalents of this machine with factor `e_factor` (for the utilisation metric).
    pub fn machine_pce(e_factor: f64) -> f64 {
        let c = nxb_sys::system::core_counts();
        f64::from(c.performance) + f64::from(c.efficiency) * e_factor
    }
}

/// `0.5 × physical − 2 GiB`, at least 1 GiB.
pub(crate) fn memory_from_physical(physical: u64) -> u64 {
    const GIB: u64 = 1 << 30;
    (physical / 2).saturating_sub(2 * GIB).max(GIB)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_rule() {
        const GIB: u64 = 1 << 30;
        assert_eq!(memory_from_physical(32 * GIB), 14 * GIB);
        assert_eq!(memory_from_physical(8 * GIB), 2 * GIB);
        assert_eq!(memory_from_physical(4 * GIB), GIB);
        assert_eq!(Budget::fixed(2).with_memory(5).mem_bytes, 5);
    }

    #[test]
    fn from_cores() {
        assert_eq!(Budget::from_cores(8, 4, 0.4, 0).tokens, 10);
        assert_eq!(Budget::from_cores(8, 4, 0.35, 0).tokens, 9);
        assert_eq!(Budget::from_cores(4, 0, 0.4, 2).tokens, 6);
        assert_eq!(Budget::from_cores(0, 0, 0.4, 0).tokens, 1);
        assert_eq!(Budget::from_cores(8, 4, f64::NAN, 0).tokens, 10);
        assert_eq!(Budget::from_cores(8, 4, 7.0, 0).tokens, 12);
    }

    #[test]
    fn detect_is_positive() {
        let b = Budget::detect(DEFAULT_E_FACTOR, 2);
        assert!(b.tokens >= 3);
        assert_eq!(b.workers, b.tokens as usize);
    }
}
