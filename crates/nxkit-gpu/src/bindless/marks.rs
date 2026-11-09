//! The IDs that shaders read through the texture pool table.
//!
//! A shader that indexes the whole ID range of the pool reads a Metal resource ID from the host table (`pool.rs` of
//! `nxb-xlate`). The host cannot know the index at encode time. Each texture read sets the bit of its ID in a bitmap
//! (`seen` in the head of the table), and the host reads the bitmap on the next frames and fills the elements of the
//! marked IDs. This file keeps the state of that reading and has no GPU in it.
//!
//! The bitmap is read every frame and cleared every [`SWEEP_EVERY`] frames. The bits since the last sweep are the
//! `current` generation, the bits of the window before are `previous`. An ID is wanted while it is in either, so an ID
//! that no shader reads leaves after two sweeps. A bit that the GPU sets while the host clears is repeated by the next
//! read of the shader, because a shader tests the bit before it sets it.

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, PoisonError};

/// The frames between two sweeps.
pub(crate) const SWEEP_EVERY: u64 = 60;

/// The marked IDs, in two generations.
#[derive(Debug, Default)]
pub(crate) struct Marks {
    /// The words of the bitmap as read in this frame: the bits set since the last sweep.
    current: Vec<u32>,
    /// The bits that the last sweep took from `current`.
    previous: Vec<u32>,
    frames: u64,
}

impl Marks {
    /// Reads the first `words` words of the bitmap with `load`, sweeps when it is time (`clear(word, bits)` clears
    /// `bits` of a word) and returns the wanted IDs in ascending order. Words that are zero are skipped.
    pub(crate) fn step(
        &mut self,
        words: usize,
        mut load: impl FnMut(usize) -> u32,
        mut clear: impl FnMut(usize, u32),
    ) -> Vec<u32> {
        self.current.clear();
        self.current.resize(words, 0);
        for (w, c) in self.current.iter_mut().enumerate() {
            *c = load(w);
        }
        self.previous.resize(words, 0);
        self.frames += 1;
        if self.frames.is_multiple_of(SWEEP_EVERY) {
            self.previous.copy_from_slice(&self.current);
            for (w, bits) in self.previous.iter().enumerate() {
                if *bits != 0 {
                    clear(w, *bits);
                }
            }
            self.current.fill(0);
        }
        let mut out = Vec::new();
        for (w, (c, p)) in self.current.iter().zip(&self.previous).enumerate() {
            let mut rest = c | p;
            while rest != 0 {
                out.push(w as u32 * 32 + rest.trailing_zeros());
                rest &= rest - 1;
            }
        }
        out
    }
}

/// What the front end and the encoder share: the wanted IDs of the last frame and the sampler epoch the encoder has
/// written. The front end copies the descriptors of the wanted IDs into the snapshot of a frame.
#[derive(Debug)]
pub(crate) struct PoolHint {
    ids: Mutex<Arc<[u32]>>,
    /// The `sampler_epoch` of the descriptors that the table of samplers holds. `u64::MAX` before the first.
    samplers: AtomicU64,
}

impl Default for PoolHint {
    fn default() -> Self {
        Self {
            ids: Mutex::new(Arc::from(Vec::new())),
            samplers: AtomicU64::new(u64::MAX),
        }
    }
}

impl PoolHint {
    /// The wanted IDs of the last frame that scanned the bitmap.
    pub(crate) fn ids(&self) -> Arc<[u32]> {
        self.ids
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Replaces the wanted IDs.
    pub(crate) fn set_ids(&self, ids: &[u32]) {
        *self.ids.lock().unwrap_or_else(PoisonError::into_inner) = Arc::from(ids);
    }

    /// The sampler epoch of the last write of the sampler table.
    pub(crate) fn sampler_epoch(&self) -> u64 {
        self.samplers.load(std::sync::atomic::Ordering::Acquire)
    }

    /// True when the sampler table lacks the samplers of `epoch`: nothing was written yet, or `epoch` is newer.
    pub(crate) fn needs_samplers(&self, epoch: u64) -> bool {
        let done = self.sampler_epoch();
        done == u64::MAX || epoch > done
    }

    /// Notes that the sampler table holds the samplers of `epoch`.
    pub(crate) fn set_sampler_epoch(&self, epoch: u64) {
        self.samplers
            .store(epoch, std::sync::atomic::Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bitmap that the test sets bits in, with the loads and clears the real one gets.
    struct Bitmap(Vec<u32>);

    impl Bitmap {
        fn set(&mut self, id: u32) {
            self.0[(id >> 5) as usize] |= 1 << (id & 31);
        }

        fn step(&mut self, m: &mut Marks, words: usize) -> Vec<u32> {
            let loads = self.0.clone();
            let mut cleared = Vec::new();
            let out = m.step(words, |w| loads[w], |w, bits| cleared.push((w, bits)));
            for (w, bits) in cleared {
                self.0[w] &= !bits;
            }
            out
        }
    }

    #[test]
    fn the_wanted_ids_are_the_current_and_the_previous_bits() {
        let mut bits = Bitmap(vec![0; 8]);
        let mut m = Marks::default();
        bits.set(3);
        bits.set(70);
        assert_eq!(bits.step(&mut m, 8), vec![3, 70]);
        bits.set(200);
        assert_eq!(bits.step(&mut m, 8), vec![3, 70, 200]);
    }

    #[test]
    fn a_sweep_moves_the_current_bits_to_the_previous_and_clears_them() {
        let mut bits = Bitmap(vec![0; 4]);
        let mut m = Marks::default();
        bits.set(5);
        for _ in 0..SWEEP_EVERY - 1 {
            bits.step(&mut m, 4);
        }
        assert_eq!(bits.0[0], 1 << 5);
        assert_eq!(bits.step(&mut m, 4), vec![5]);
        // The sweep took the bit from the bitmap, and the ID is still wanted from the previous generation.
        assert_eq!(bits.0, vec![0; 4]);
        assert_eq!(bits.step(&mut m, 4), vec![5]);
    }

    #[test]
    fn an_id_that_is_not_marked_for_two_sweeps_leaves() {
        let mut bits = Bitmap(vec![0; 4]);
        let mut m = Marks::default();
        bits.set(5);
        bits.set(40);
        for _ in 0..SWEEP_EVERY {
            bits.step(&mut m, 4);
        }
        // 40 is read again, 5 is not.
        bits.set(40);
        for _ in 0..SWEEP_EVERY - 1 {
            assert!(bits.step(&mut m, 4).contains(&5));
        }
        // The second sweep: 5 was marked in no window since the first sweep.
        assert_eq!(bits.step(&mut m, 4), vec![40]);
        assert_eq!(bits.step(&mut m, 4), vec![40]);
    }

    #[test]
    fn the_scan_stops_at_the_requested_words_and_skips_nothing_below() {
        let mut bits = Bitmap(vec![0; 8]);
        let mut m = Marks::default();
        bits.set(31);
        bits.set(32);
        bits.set(100);
        let mut loaded = Vec::new();
        let out = m.step(
            2,
            |w| {
                loaded.push(w);
                bits.0[w]
            },
            |_, _| {},
        );
        assert_eq!(loaded, vec![0, 1]);
        assert_eq!(out, vec![31, 32]);
    }

    #[test]
    fn a_sweep_clears_only_the_words_with_bits() {
        let mut bits = Bitmap(vec![0; 4]);
        let mut m = Marks::default();
        bits.set(65);
        let mut clears = Vec::new();
        for _ in 0..SWEEP_EVERY {
            m.step(4, |w| bits.0[w], |w, b| clears.push((w, b)));
        }
        assert_eq!(clears, vec![(2, 2)]);
    }

    #[test]
    fn the_hint_keeps_the_ids_and_the_sampler_epoch() {
        let h = PoolHint::default();
        assert!(h.ids().is_empty());
        assert_eq!(h.sampler_epoch(), u64::MAX);
        h.set_ids(&[1, 2]);
        h.set_sampler_epoch(4);
        assert_eq!(&*h.ids(), &[1, 2]);
        assert_eq!(h.sampler_epoch(), 4);
    }
}
