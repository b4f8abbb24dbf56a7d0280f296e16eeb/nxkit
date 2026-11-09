//! The weights of a layer reordered into the layout the kernels read.
//!
//! The game's blob holds each layer as f16 `[tap][cin][cout]` (cout fastest) and its bias as f16 `[cout]`. The kernels
//! read the weights as 8x8 tiles for `simdgroup_load` with a row stride of 8, so a tile is 64 contiguous halves, row =
//! input channel, column = output channel, and the tiles that one step of the chain needs sit next to each other:
//!
//! - tile `(tap, k8, n8)` is at tile index `(tap * (cin / 8) + k8) * (cout / 8) + n8`, 128 bytes;
//! - the bias follows as `cout / 8` tiles whose 8 rows are all the 8 biases of the tile, so that a `simdgroup_load` gives
//!   a matrix with the bias in each row, in whatever layout the lanes hold their elements.
//!
//! The functions move bytes (two at a time) and never convert, so they are exact for every f16 pattern.

/// Bytes of one 8x8 f16 tile.
pub const TILE_BYTES: usize = 128;

/// The bytes of a packed layer: the weight tiles then the bias tiles.
#[must_use]
pub fn packed_len(taps: usize, cin: usize, cout: usize) -> usize {
    taps * cin * cout * 2 + cout * 8 * 2
}

/// The weights of a layer as tiles, from the game's `[tap][cin][cout]` f16 bytes.
///
/// # Errors
///
/// When `cin` or `cout` is not a multiple of 8, or `weights` is not exactly `taps * cin * cout * 2` bytes.
pub fn pack_weights(
    weights: &[u8],
    taps: usize,
    cin: usize,
    cout: usize,
) -> Result<Vec<u8>, String> {
    if !cin.is_multiple_of(8) || !cout.is_multiple_of(8) || taps == 0 || cin == 0 || cout == 0 {
        return Err(format!("channels {cin} -> {cout} are not multiples of 8"));
    }
    if weights.len() != taps * cin * cout * 2 {
        return Err(format!(
            "{} weight bytes, expected {}",
            weights.len(),
            taps * cin * cout * 2
        ));
    }
    let (k8s, n8s) = (cin / 8, cout / 8);
    let mut out = vec![0u8; weights.len()];
    for tap in 0..taps {
        for k8 in 0..k8s {
            for n8 in 0..n8s {
                let tile = (tap * k8s + k8) * n8s + n8;
                for r in 0..8 {
                    let src = ((tap * cin + k8 * 8 + r) * cout + n8 * 8) * 2;
                    let dst = tile * TILE_BYTES + r * 16;
                    out[dst..dst + 16].copy_from_slice(&weights[src..src + 16]);
                }
            }
        }
    }
    Ok(out)
}

/// The bias of a layer as `cout / 8` tiles with the 8 biases of the tile in each of its 8 rows.
///
/// # Errors
///
/// When `cout` is not a multiple of 8 or `bias` is shorter than `cout * 2` bytes (a longer slice is allowed: the blob
/// pads the bias to 0x100 bytes).
pub fn pack_bias(bias: &[u8], cout: usize) -> Result<Vec<u8>, String> {
    if cout == 0 || !cout.is_multiple_of(8) {
        return Err(format!("{cout} output channels is not a multiple of 8"));
    }
    if bias.len() < cout * 2 {
        return Err(format!("{} bias bytes, expected {}", bias.len(), cout * 2));
    }
    let mut out = Vec::with_capacity(cout * 8 * 2);
    for n8 in 0..cout / 8 {
        for _ in 0..8 {
            out.extend_from_slice(&bias[n8 * 16..n8 * 16 + 16]);
        }
    }
    Ok(out)
}

/// A whole layer for one buffer: [`pack_weights`] then [`pack_bias`]. `weights` and `bias` are the slices of the game's
/// blob at the layer's offsets; `bias` may run past the layer.
///
/// # Errors
///
/// As [`pack_weights`] and [`pack_bias`].
pub fn pack_layer(
    weights: &[u8],
    bias: &[u8],
    taps: usize,
    cin: usize,
    cout: usize,
) -> Result<Vec<u8>, String> {
    let mut out = pack_weights(weights, taps, cin, cout)?;
    out.extend_from_slice(&pack_bias(bias, cout)?);
    debug_assert_eq!(out.len(), packed_len(taps, cin, cout));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Element `i` of a fake tensor as the two bytes of the half `i`.
    fn counting(n: usize) -> Vec<u8> {
        (0..n)
            .flat_map(|i| u16::try_from(i).unwrap_or(0).to_le_bytes())
            .collect()
    }
    fn half_at(bytes: &[u8], i: usize) -> u16 {
        u16::from_le_bytes([bytes[2 * i], bytes[2 * i + 1]])
    }

    #[test]
    fn a_tile_holds_rows_of_input_channels() {
        // 2 taps, 16 -> 24: tiles per tap are 2 (k8) x 3 (n8)
        let (taps, cin, cout) = (2usize, 16usize, 24usize);
        let w = counting(taps * cin * cout);
        let p = pack_weights(&w, taps, cin, cout).expect("packs");
        assert_eq!(p.len(), w.len());
        for tap in 0..taps {
            for k8 in 0..2 {
                for n8 in 0..3 {
                    let tile = (tap * 2 + k8) * 3 + n8;
                    for r in 0..8 {
                        for c in 0..8 {
                            let got = half_at(&p, tile * 64 + r * 8 + c);
                            let want = (tap * cin + k8 * 8 + r) * cout + n8 * 8 + c;
                            assert_eq!(
                                usize::from(got),
                                want,
                                "tap {tap} k8 {k8} n8 {n8} r {r} c {c}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_bias_tile_repeats_the_bias_in_every_row() {
        let b = counting(40);
        let p = pack_bias(&b, 16).expect("packs");
        assert_eq!(p.len(), 16 * 8 * 2);
        for n8 in 0..2 {
            for r in 0..8 {
                for c in 0..8 {
                    assert_eq!(usize::from(half_at(&p, n8 * 64 + r * 8 + c)), n8 * 8 + c);
                }
            }
        }
    }

    #[test]
    fn layer_length_and_errors() {
        let w = counting(9 * 16 * 16);
        let b = counting(16);
        let p = pack_layer(&w, &b, 9, 16, 16).expect("packs");
        assert_eq!(p.len(), packed_len(9, 16, 16));
        assert!(pack_weights(&w[..10], 9, 16, 16).is_err());
        assert!(pack_weights(&w, 9, 12, 16).is_err());
        assert!(pack_bias(&b[..8], 16).is_err());
    }
}
