//! The console the game was built for: the Switch ("NX") or the Switch 2 ("Ounce").
//!
//! The host finds it once at load, from the `sdk` module, and sets it with [`set`] before the game runs. The kernel,
//! the NVN front end and the GPU backend read it with [`get`]. The two consoles differ in the rate of the system
//! counter, the values the NVN device reports and the order of the bytes inside a GOB of a block-linear texture.

use std::sync::atomic::{AtomicU8, Ordering};

/// The console the game was built for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Platform {
    /// The Switch. The default until the host has looked at the game.
    #[default]
    Nx,
    /// The Switch 2.
    Ounce,
}

impl Platform {
    /// The rate of the system counter in Hz: 19.2 MHz on the Switch, 31.25 MHz (32 ns a tick) on the Switch 2. The SDK
    /// of the Switch 2 reads `cntpct_el0` and converts with that rate.
    pub const fn tick_hz(self) -> u64 {
        match self {
            Platform::Nx => 19_200_000,
            Platform::Ounce => 31_250_000,
        }
    }

    /// The system counter ticks for a time in nanoseconds, rounded down.
    pub fn ticks_from_ns(self, ns: u64) -> u64 {
        (u128::from(ns) * u128::from(self.tick_hz()) / 1_000_000_000) as u64
    }

    /// The rate of the time in a GPU report (`NVNcounterData`) in Hz. On the Switch a tick is 625/384 ns (614.4 MHz).
    /// On the Switch 2 a report tick is a system tick: 31.25 MHz, 32 ns a tick, on the same base as the system counter.
    pub const fn report_hz(self) -> u64 {
        match self {
            Platform::Nx => 614_400_000,
            Platform::Ounce => 31_250_000,
        }
    }

    /// The report ticks for a time in nanoseconds: x 384 / 625 on the Switch, / 32 on the Switch 2. It rounds down and
    /// saturates at `u64::MAX`.
    pub fn report_ticks_from_ns(self, ns: u64) -> u64 {
        let ticks = match self {
            Platform::Nx => u128::from(ns) * 384 / 625,
            Platform::Ounce => u128::from(ns) / 32,
        };
        u64::try_from(ticks).unwrap_or(u64::MAX)
    }

    /// The nanoseconds for report ticks: x 625 / 384 on the Switch, x 32 on the Switch 2. It rounds down and saturates at
    /// `u64::MAX`. This is the conversion of `nvnDeviceGetTimestampInNanoseconds`.
    pub fn report_ns_from_ticks(self, ticks: u64) -> u64 {
        let ns = match self {
            Platform::Nx => u128::from(ticks) * 625 / 384,
            Platform::Ounce => u128::from(ticks) * 32,
        };
        u64::try_from(ns).unwrap_or(u64::MAX)
    }
}

static PLATFORM: AtomicU8 = AtomicU8::new(0);

/// Sets the console of the game. The host calls it once, after the modules are loaded and before the game runs.
pub fn set(p: Platform) {
    PLATFORM.store(p as u8, Ordering::Relaxed);
}

/// The console of the game. [`Platform::Nx`] before [`set`].
pub fn get() -> Platform {
    match PLATFORM.load(Ordering::Relaxed) {
        1 => Platform::Ounce,
        _ => Platform::Nx,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ticks_follow_the_rate_of_the_console() {
        assert_eq!(Platform::Nx.ticks_from_ns(1_000_000_000), 19_200_000);
        assert_eq!(Platform::Nx.ticks_from_ns(625), 12);
        assert_eq!(Platform::Ounce.ticks_from_ns(1_000_000_000), 31_250_000);
        assert_eq!(Platform::Ounce.ticks_from_ns(64), 2);
        assert_eq!(Platform::Ounce.ticks_from_ns(u64::MAX), u64::MAX / 32);
    }

    #[test]
    fn a_second_is_the_report_rate_in_ticks() {
        assert_eq!(
            Platform::Ounce.report_ticks_from_ns(1_000_000_000),
            31_250_000
        );
        assert_eq!(
            Platform::Nx.report_ticks_from_ns(1_000_000_000),
            614_400_000
        );
        assert_eq!(Platform::Ounce.report_hz(), 31_250_000);
        assert_eq!(Platform::Nx.report_hz(), 614_400_000);
        assert_eq!(
            Platform::Ounce.report_ns_from_ticks(31_250_000),
            1_000_000_000
        );
        assert_eq!(
            Platform::Nx.report_ns_from_ticks(614_400_000),
            1_000_000_000
        );
        assert_eq!(Platform::Ounce.report_ns_from_ticks(1), 32);
        assert_eq!(Platform::Nx.report_ns_from_ticks(384), 625);
    }

    #[test]
    fn the_report_ticks_round_trip() {
        // Every tick of the Switch 2 is a whole number of nanoseconds. On the Switch a whole number of nanoseconds needs
        // a multiple of 384 ticks.
        for ticks in [0, 1, 31_250_000, 1 << 50] {
            let p = Platform::Ounce;
            assert_eq!(p.report_ticks_from_ns(p.report_ns_from_ticks(ticks)), ticks);
        }
        for ticks in [0, 384, 384 * 1000, 614_400_000, 384 << 30] {
            let p = Platform::Nx;
            assert_eq!(p.report_ticks_from_ns(p.report_ns_from_ticks(ticks)), ticks);
        }
        // A time that is no whole number of ticks rounds down and never up.
        assert_eq!(
            Platform::Ounce.report_ns_from_ticks(Platform::Ounce.report_ticks_from_ns(95)),
            64
        );
    }

    #[test]
    fn the_report_ticks_do_not_overflow() {
        // x 32 and x 625 / 384 of the largest tick count do not fit u64 and saturate.
        assert_eq!(Platform::Ounce.report_ns_from_ticks(u64::MAX), u64::MAX);
        assert_eq!(Platform::Nx.report_ns_from_ticks(u64::MAX), u64::MAX);
        assert_eq!(
            Platform::Ounce.report_ticks_from_ns(u64::MAX),
            u64::MAX / 32
        );
        assert_eq!(
            Platform::Nx.report_ticks_from_ns(u64::MAX),
            (u128::from(u64::MAX) * 384 / 625) as u64
        );
    }
}
