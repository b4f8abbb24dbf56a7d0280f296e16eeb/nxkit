//! What the frame step learns about the device: the CPU parked on slow cores and the thermal cap.
//!
//! Ported from `fstep_cpu_slowed` and `thermal_capped` in `dm/gfx/nvn/hle_nvn.c`, read on 2026-11-07. The
//! host reads the counters and the thermal state of the system and gives them to the service. The service decides.

/// The calm time after which a thermal cap ends (30 s).
pub const THERMAL_CALM_NS: u64 = 30_000_000_000;
/// The thermal state is read once a second.
const THERMAL_SAMPLE_NS: u64 = 1_000_000_000;
/// The thermal state from which the cap holds: serious (2) or worse.
pub const THERMAL_SERIOUS: i32 = 2;
/// The presents between two windows of the CPU share.
const CPU_WINDOW: u64 = 30;
/// A share of the CPU time on performance cores below this many percent is low.
const LOW_SHARE: i32 = 30;

/// The CPU time of the process in mach time units, in total and on performance cores.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PerfCounters {
    /// All CPU time (user and system).
    pub total: u64,
    /// The time on performance cores.
    pub perf: u64,
}

/// Whether the CPU runs below its speed for a reason other than heat.
///
/// It is so when less than 30 percent of the process's CPU time ran on performance cores in each of the last two windows
/// of 30 presents, on a system with two core types, at thermal state nominal or fair and with Low Power Mode off. One
/// window alone dips that low on a Mac at 30 fps now and then (26 percent once in 70 s, 97 percent around it).
#[derive(Clone, Debug)]
pub struct CpuWatch {
    levels: u32,
    slowed: bool,
    low_was: bool,
    last: PerfCounters,
    share: Option<i32>,
}

impl CpuWatch {
    /// A watch for a system with `perf_levels` core types (`hw.nperflevels`). One level never reports slow.
    pub fn new(perf_levels: u32) -> Self {
        Self {
            levels: perf_levels,
            slowed: false,
            low_was: false,
            last: PerfCounters::default(),
            share: None,
        }
    }

    /// The share of the last window in percent, when one was measured.
    pub fn share(&self) -> Option<i32> {
        self.share
    }

    /// Runs once a present. `perf` is the counters now, or `None` when the system did not give them. They are read in the
    /// presents that are a multiple of 30 only.
    pub fn slowed(
        &mut self,
        presents: u64,
        perf: Option<PerfCounters>,
        thermal: i32,
        low_power: bool,
    ) -> bool {
        if self.levels < 2 || !presents.is_multiple_of(CPU_WINDOW) {
            return self.levels >= 2 && self.slowed;
        }
        let Some(now) = perf else {
            return self.slowed;
        };
        if self.last.total != 0 && now.total > self.last.total {
            let used = now.total - self.last.total;
            let on_perf = now.perf.saturating_sub(self.last.perf);
            self.share =
                Some(i32::try_from(on_perf.saturating_mul(100) / used).unwrap_or(i32::MAX));
        }
        self.last = now;
        let low = self.share.is_some_and(|s| s < LOW_SHARE);
        self.slowed = low && self.low_was && thermal <= 1 && !low_power;
        self.low_was = low;
        self.slowed
    }
}

/// The thermal cap of a phone: while the device is at thermal state serious or worse the frame step holds 30 fps. It goes
/// back to the cap once the device was fair or better for 30 s, so it does not flap. At serious, iOS lowers the GPU
/// clock to about a third, and a steady 30 fps also lets the device cool.
#[derive(Clone, Debug)]
pub struct ThermalCap {
    enabled: bool,
    capped: bool,
    last: Option<u64>,
    calm_since: Option<u64>,
}

impl ThermalCap {
    /// A cap that is on or off. A system without a thermal state (a Mac) never reaches serious.
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            capped: false,
            last: None,
            calm_since: None,
        }
    }

    /// Whether the cap holds now. `thermal` is the state of the system at `now_ns`, a monotonic time. The state counts
    /// once a second.
    pub fn capped(&mut self, now_ns: u64, thermal: i32) -> bool {
        let due = self
            .last
            .is_none_or(|l| now_ns.saturating_sub(l) >= THERMAL_SAMPLE_NS);
        if self.enabled && due {
            self.last = Some(now_ns);
            if thermal >= THERMAL_SERIOUS {
                self.calm_since = None;
                self.capped = true;
            } else if self.capped {
                match self.calm_since {
                    None => self.calm_since = Some(now_ns),
                    Some(c) if now_ns.saturating_sub(c) >= THERMAL_CALM_NS => self.capped = false,
                    Some(_) => {}
                }
            }
        }
        self.capped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(total: u64, perf: u64) -> Option<PerfCounters> {
        Some(PerfCounters { total, perf })
    }

    #[test]
    fn two_low_windows_in_a_row_make_the_cpu_slow() {
        let mut w = CpuWatch::new(2);
        assert!(!w.slowed(30, window(1000, 900), 0, false));
        assert!(
            !w.slowed(60, window(2000, 1000), 0, false),
            "10 percent once"
        );
        assert_eq!(w.share(), Some(10));
        assert!(
            w.slowed(90, window(3000, 1100), 0, false),
            "10 percent twice"
        );
        assert!(
            w.slowed(91, None, 0, false),
            "between windows the answer stays"
        );
        assert!(
            !w.slowed(120, window(4000, 2100), 0, false),
            "a window above 30 ends it"
        );
    }

    #[test]
    fn heat_low_power_and_one_core_type_never_report_slow() {
        let mut hot = CpuWatch::new(2);
        let mut low = CpuWatch::new(2);
        let mut one = CpuWatch::new(1);
        for i in 1..=4u64 {
            let c = window(i * 1000, i * 50);
            assert!(!hot.slowed(i * 30, c, 2, false));
            assert!(!low.slowed(i * 30, c, 0, true));
            assert!(!one.slowed(i * 30, c, 0, false));
        }
    }

    #[test]
    fn the_thermal_cap_holds_until_the_device_was_calm_for_thirty_seconds() {
        let s = 1_000_000_000u64;
        let mut t = ThermalCap::new(true);
        assert!(!t.capped(10 * s, 1));
        assert!(t.capped(11 * s, 2));
        assert!(t.capped(12 * s, 1), "calm starts");
        assert!(t.capped(41 * s, 1), "29 s calm");
        assert!(!t.capped(42 * s, 1), "30 s calm");
        assert!(t.capped(43 * s, 3));
        assert!(t.capped(43 * s + 1, 0), "one sample a second");
    }

    #[test]
    fn a_cap_that_is_off_never_holds() {
        let mut t = ThermalCap::new(false);
        assert!(!t.capped(10_000_000_000, 3));
    }
}
