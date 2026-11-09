//! The entropy of the process: one stream for `getentropy`, the csrng take-over and `GetInfo` `RandomEntropy`
//!.
//!
//! The stream is random by default. With [`set_deterministic`] it is a fixed-seed splitmix64 stream, so two runs
//! of a replay get the same bytes.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const SEED: u64 = 0x5357_4445_5452_4e47;
const GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

static DETERMINISTIC: AtomicBool = AtomicBool::new(false);
static STATE: AtomicU64 = AtomicU64::new(SEED);
static CALLS: AtomicU64 = AtomicU64::new(0);

/// Switches the fixed-seed stream on or off. Turning it on starts the stream from its seed.
pub fn set_deterministic(on: bool) {
    if on {
        STATE.store(SEED, Ordering::Relaxed);
        CALLS.store(0, Ordering::Relaxed);
    }
    DETERMINISTIC.store(on, Ordering::Release);
}

/// The splitmix64 output for a stream state.
fn mix(state: u64) -> u64 {
    let z = (state ^ (state >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    let z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The next 64 bits.
pub fn next64() -> u64 {
    if !DETERMINISTIC.load(Ordering::Acquire) {
        let mut word = [0u8; 8];
        crate::sys::random_fill(&mut word);
        return u64::from_le_bytes(word);
    }
    let state = STATE
        .fetch_add(GAMMA, Ordering::Relaxed)
        .wrapping_add(GAMMA);
    let n = CALLS.fetch_add(1, Ordering::Relaxed) + 1;
    if n == 1 || n.is_multiple_of(1000) {
        eprintln!("nxkit-hos: deterministic entropy call {n}");
    }
    mix(state)
}

/// Fills `buf`.
pub fn fill(buf: &mut [u8]) {
    if !DETERMINISTIC.load(Ordering::Acquire) {
        crate::sys::random_fill(buf);
        return;
    }
    for chunk in buf.chunks_mut(8) {
        let word = next64().to_le_bytes();
        chunk.copy_from_slice(&word[..chunk.len()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_is_a_function_of_the_state() {
        let a = mix(SEED.wrapping_add(GAMMA));
        let b = mix(SEED.wrapping_add(GAMMA).wrapping_add(GAMMA));
        assert_ne!(a, b);
        assert_eq!(a, mix(SEED.wrapping_add(GAMMA)));
    }

    #[test]
    fn fill_covers_a_length_that_is_not_a_multiple_of_eight() {
        let mut buf = [0u8; 13];
        fill(&mut buf);
        assert!(buf.iter().any(|b| *b != 0));
    }
}
