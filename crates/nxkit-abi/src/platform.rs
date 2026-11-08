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
}
