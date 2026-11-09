//! The six-layer separable network (section 7.3) on the vr weight layout (section 8.2).

use super::arith::{Mat, hmma, relu};
use crate::half;

/// Number of layers of a vr network.
pub const LAYERS: usize = 6;
/// Bytes of a vr weight blob.
pub const BLOB_LEN: usize = LAYERS * crate::net::LAYER_BYTES;

/// Element index of `W1_s[r][k]` in the vr blob, in halves.
pub fn w1_index(s: usize, r: usize, k: usize) -> usize {
    512 * s + 8 * (4 * (r % 8) + (k % 8) / 2) + 2 * (r / 8 + 2 * (k / 8)) + k % 2
}

/// Element index of `W2_s[k][n]` in the vr blob, in halves.
pub fn w2_index(s: usize, k: usize, n: usize) -> usize {
    512 * s + 256 + 8 * (4 * (n % 8) + (k % 8) / 2) + 4 * (n / 8) + 2 * (k / 8) + k % 2
}

/// The twelve sub-blocks of a vr blob as matrices.
#[derive(Clone, Debug)]
pub struct Weights {
    w1: Vec<Mat>,
    w2: Vec<Mat>,
}

impl Weights {
    /// Decode a vr blob of [`BLOB_LEN`] bytes; `None` for any other length.
    pub fn from_vr_blob(blob: &[u8]) -> Option<Weights> {
        if blob.len() != BLOB_LEN {
            return None;
        }
        let at = |i: usize| half::to_f32(u16::from_le_bytes([blob[2 * i], blob[2 * i + 1]]));
        let build = |f: &dyn Fn(usize, usize, usize) -> usize| -> Vec<Mat> {
            (0..2 * LAYERS)
                .map(|s| std::array::from_fn(|i| std::array::from_fn(|j| at(f(s, i, j)))))
                .collect()
        };
        Some(Weights {
            w1: build(&w1_index),
            w2: build(&w2_index),
        })
    }
}

/// The network on one patch. `x[k][n]` is the input with `k` along x and `n` along y; the result `y[r][c]` has `r`
/// along y and `c` along x. Every `f16()` of the notes is one [`hmma`]; there is a ReLU and a transpose between layers.
pub fn network(w: &Weights, x: &Mat) -> Mat {
    let mut z = *x;
    let mut d = [[0.0; 16]; 16];
    for l in 0..LAYERS {
        let mut acc: Option<Mat> = None;
        for t in 0..2 {
            let s = 2 * l + t;
            let h1 = hmma(&w.w1[s], &z, None);
            acc = Some(hmma(&h1, &w.w2[s], acc.as_ref()));
        }
        d = acc.unwrap_or(d);
        if l < LAYERS - 1 {
            z = std::array::from_fn(|k| std::array::from_fn(|n| relu(d[n][k])));
        }
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn the_index_maps_are_permutations_of_one_layer_pair() {
        let mut seen = HashSet::new();
        for s in 0..2 {
            for a in 0..16 {
                for b in 0..16 {
                    assert!(seen.insert(w1_index(s, a, b)));
                    assert!(seen.insert(w2_index(s, a, b)));
                }
            }
        }
        assert_eq!(seen.len(), 1024);
        assert_eq!(seen.iter().max(), Some(&1023));
    }

    #[test]
    fn fragment_order_of_the_first_lane() {
        // Lane 0 (g = 0, t = 0) loads halves 0..8 of W1_0: registers (row 0, k 0), (row 8, k 0), (row 0, k 8),
        // (row 8, k 8), two halves each (k, k + 1).
        assert_eq!(w1_index(0, 0, 0), 0);
        assert_eq!(w1_index(0, 0, 1), 1);
        assert_eq!(w1_index(0, 8, 0), 2);
        assert_eq!(w1_index(0, 0, 8), 4);
        assert_eq!(w1_index(0, 8, 8), 6);
        // Lane 1 (t = 1) starts at half 8 with k = 2.
        assert_eq!(w1_index(0, 0, 2), 8);
    }

    #[test]
    fn wrong_length_is_refused() {
        assert!(Weights::from_vr_blob(&[0; 0x1800]).is_none());
        assert!(Weights::from_vr_blob(&vec![0; BLOB_LEN]).is_some());
    }

    #[test]
    fn zero_weights_give_zero() {
        let w = Weights::from_vr_blob(&vec![0; BLOB_LEN]).unwrap();
        let x = [[0.5; 16]; 16];
        assert_eq!(network(&w, &x), [[0.0; 16]; 16]);
    }
}
