//! The weights of the network in the layout the kernel reads.
//!
//! One layout serves every pack (`docs/plans/ndsr-native.md` section 8.3): the canonical one, which is agl_ndsr's and
//! `nn::ndve`'s resampler's. A layer is 1024 halves, `[W1_0, W1_1, W2_0, W2_1]` of 256 halves each, as 8x8 row-major
//! tiles ready for `simdgroup_load` with a row stride of 8:
//!
//! - `W1_t[r][k]` at `256 t + 128 (k / 8) + 64 (r / 8) + 8 (r % 8) + k % 8`;
//! - `W2_t[k][n]` at `512 + 256 t + 128 (n / 8) + 64 (k / 8) + 8 (k % 8) + n % 8`.
//!
//! The vr blobs are the same matrices in the order of the tensor core fragments (section 8.2), and the even layers hold
//! the transposed and swapped matrices (section 8.3): [`from_vr`] undoes both. The values are the same, but the order of
//! the two products of an even layer is not: the vr programs contract with W2 first there, which rounds differently in
//! f16 from agl's W1-first order, so the kernel runs the vr order (see the test `repacked_blob_gives_the_same_network`).
//! No biases exist. A blob is six layers
//! (0x3000 bytes) or, for the 4/3 models of agl_ndsr and vr_light, three (0x1800). The functions move values and never
//! convert them, so they are exact for every f16 pattern.

/// Halves of one layer: four 16x16 matrices.
pub const LAYER_HALVES: usize = 1024;
/// Bytes of one layer.
pub const LAYER_BYTES: usize = 2 * LAYER_HALVES;
/// Layers of a full network.
pub const LAYERS: usize = 6;
/// Bytes of a blob of six layers.
pub const BLOB_BYTES: usize = LAYERS * LAYER_BYTES;
/// Bytes of the blob of the 3-layer models.
pub const SHORT_BLOB_BYTES: usize = 3 * LAYER_BYTES;

/// Index in a layer (canonical layout) of `W1_t[r][k]`.
#[must_use]
pub const fn w1_index(t: usize, r: usize, k: usize) -> usize {
    256 * t + 128 * (k / 8) + 64 * (r / 8) + 8 * (r % 8) + k % 8
}

/// Index in a layer (canonical layout) of `W2_t[k][n]`.
#[must_use]
pub const fn w2_index(t: usize, k: usize, n: usize) -> usize {
    512 + 256 * t + 128 * (n / 8) + 64 * (k / 8) + 8 * (k % 8) + n % 8
}

/// Index in a vr blob of `W1_s[r][k]`, sub-block `s` (ndsr-native.md section 8.2).
#[must_use]
pub const fn vr_w1_index(s: usize, r: usize, k: usize) -> usize {
    512 * s + 8 * (4 * (r % 8) + (k % 8) / 2) + 2 * (r / 8 + 2 * (k / 8)) + k % 2
}

/// Index in a vr blob of `W2_s[k][n]`, sub-block `s` (ndsr-native.md section 8.2).
#[must_use]
pub const fn vr_w2_index(s: usize, k: usize, n: usize) -> usize {
    512 * s + 256 + 8 * (4 * (n % 8) + (k % 8) / 2) + 4 * (n / 8) + 2 * (k / 8) + k % 2
}

fn check_len(blob: &[u8], what: &str, lengths: &[usize]) -> Result<usize, String> {
    if lengths.contains(&blob.len()) {
        Ok(blob.len() / LAYER_BYTES)
    } else {
        Err(format!(
            "a {what} blob is {} bytes, expected one of {lengths:x?}",
            blob.len()
        ))
    }
}

/// The weights of an agl_ndsr (or NDVE resampler) blob: already canonical, so a checked copy.
///
/// # Errors
///
/// When the blob is not 0x3000 or 0x1800 bytes.
pub fn from_agl(blob: &[u8]) -> Result<Vec<u8>, String> {
    check_len(blob, "agl", &[BLOB_BYTES, SHORT_BLOB_BYTES])?;
    Ok(blob.to_vec())
}

/// The canonical weights of a vr blob.
///
/// # Errors
///
/// When the blob is not 0x3000 or 0x1800 bytes. The 3-layer blob of vr_light's 4/3 model is in the vr layout too: its
/// repack is agl_ndsr's 4/3 blob, every value (ndsr-native.md section 15.2).
pub fn from_vr(blob: &[u8]) -> Result<Vec<u8>, String> {
    let layers = check_len(blob, "vr", &[BLOB_BYTES, SHORT_BLOB_BYTES])?;
    let src = |i: usize| [blob[2 * i], blob[2 * i + 1]];
    let mut out = vec![0u8; blob.len()];
    for layer in 0..layers {
        let base = layer * LAYER_HALVES;
        let mut put = |dst: usize, from: usize| {
            out[2 * (base + dst)..2 * (base + dst) + 2].copy_from_slice(&src(from));
        };
        for t in 0..2 {
            let s = 2 * layer + t;
            for a in 0..16 {
                for b in 0..16 {
                    if layer % 2 == 1 {
                        // odd layers are the same matrices
                        put(w1_index(t, a, b), vr_w1_index(s, a, b));
                        put(w2_index(t, a, b), vr_w2_index(s, a, b));
                    } else {
                        // even layers: vrW1 = W2^T and vrW2 = W1^T
                        put(w2_index(t, a, b), vr_w1_index(s, b, a));
                        put(w1_index(t, a, b), vr_w2_index(s, b, a));
                    }
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The half `i` as its two bytes (a counting blob: every half different).
    fn counting(n: usize) -> Vec<u8> {
        (0..n)
            .flat_map(|i| u16::try_from(i).unwrap_or(0).to_le_bytes())
            .collect()
    }
    fn half_at(bytes: &[u8], i: usize) -> usize {
        usize::from(u16::from_le_bytes([bytes[2 * i], bytes[2 * i + 1]]))
    }

    /// The indices are bijections onto a layer / a vr sub-block pair.
    #[test]
    fn index_maps_are_permutations() {
        let mut canon = vec![false; LAYER_HALVES];
        let mut vr = vec![false; LAYER_HALVES];
        for t in 0..2 {
            for a in 0..16 {
                for b in 0..16 {
                    for map in [w1_index(t, a, b), w2_index(t, a, b)] {
                        assert!(!canon[map], "index {map} twice");
                        canon[map] = true;
                    }
                    for map in [vr_w1_index(t, a, b), vr_w2_index(t, a, b)] {
                        assert!(!vr[map], "index {map} twice");
                        vr[map] = true;
                    }
                }
            }
        }
        assert!(canon.iter().all(|&b| b) && vr.iter().all(|&b| b));
    }

    /// Every value lands where section 8.3 says: odd layers verbatim, even layers transposed and swapped.
    #[test]
    fn vr_maps_to_canonical_by_section_8_3() {
        let blob = counting(BLOB_BYTES / 2);
        let c = from_vr(&blob).expect("packs");
        assert_eq!(c.len(), BLOB_BYTES);
        let mut seen = vec![false; BLOB_BYTES / 2];
        for layer in 0..LAYERS {
            for t in 0..2 {
                let s = 2 * layer + t;
                for a in 0..16 {
                    for b in 0..16 {
                        let base = layer * LAYER_HALVES;
                        let (w1, w2) = (base + w1_index(t, a, b), base + w2_index(t, a, b));
                        let (v1, v2) = if layer % 2 == 1 {
                            (vr_w1_index(s, a, b), vr_w2_index(s, a, b))
                        } else {
                            (vr_w2_index(s, b, a), vr_w1_index(s, b, a))
                        };
                        assert_eq!(half_at(&c, w1), v1, "W1 layer {layer} t {t} [{a}][{b}]");
                        assert_eq!(half_at(&c, w2), v2, "W2 layer {layer} t {t} [{a}][{b}]");
                        seen[w1] = true;
                        seen[w2] = true;
                    }
                }
            }
        }
        assert!(seen.iter().all(|&b| b), "every canonical slot is filled");
    }

    #[test]
    fn lengths() {
        assert_eq!(
            from_agl(&vec![1; SHORT_BLOB_BYTES]).map(|v| v.len()),
            Ok(SHORT_BLOB_BYTES)
        );
        assert_eq!(
            from_agl(&vec![1; BLOB_BYTES]).map(|v| v.len()),
            Ok(BLOB_BYTES)
        );
        assert!(from_agl(&[0; 10]).is_err());
        assert_eq!(
            from_vr(&vec![0; SHORT_BLOB_BYTES]).map(|v| v.len()),
            Ok(SHORT_BLOB_BYTES)
        );
        assert!(from_vr(&[]).is_err());
    }

    // The network in both forms on exact numbers, to show that the repack keeps the arithmetic (section 8.3).

    /// `x` rounded to the nearest f16 (ties to even), as an f64.
    fn f16r(x: f64) -> f64 {
        if x == 0.0 || !x.is_finite() {
            return x;
        }
        let e = x.abs().log2().floor().max(-14.0);
        let ulp = (e - 10.0).exp2();
        let q = x / ulp;
        let r = q.round();
        let r = if (q - q.trunc()).abs() == 0.5 && r % 2.0 != 0.0 {
            r - q.signum()
        } else {
            r
        };
        (r * ulp).clamp(-65504.0, 65504.0)
    }
    /// The f16 pattern's value for the small multiples of 1/64 the tests use.
    fn mat(
        blob_val: &dyn Fn(usize) -> f64,
        idx: impl Fn(usize, usize) -> usize,
    ) -> [[f64; 16]; 16] {
        let mut m = [[0.0; 16]; 16];
        for (r, row) in m.iter_mut().enumerate() {
            for (c, v) in row.iter_mut().enumerate() {
                *v = blob_val(idx(r, c));
            }
        }
        m
    }
    /// `D = f16(A B + C)` per element, in exact arithmetic.
    fn hmma(
        a: &[[f64; 16]; 16],
        b: &[[f64; 16]; 16],
        c: Option<&[[f64; 16]; 16]>,
    ) -> [[f64; 16]; 16] {
        let mut d = [[0.0; 16]; 16];
        for r in 0..16 {
            for n in 0..16 {
                let mut s = c.map_or(0.0, |c| c[r][n]);
                for k in 0..16 {
                    s += a[r][k] * b[k][n];
                }
                d[r][n] = f16r(s);
            }
        }
        d
    }
    fn transpose(m: &[[f64; 16]; 16]) -> [[f64; 16]; 16] {
        let mut t = [[0.0; 16]; 16];
        for r in 0..16 {
            for c in 0..16 {
                t[c][r] = m[r][c];
            }
        }
        t
    }
    fn relu(m: &[[f64; 16]; 16]) -> [[f64; 16]; 16] {
        m.map(|row| row.map(|v| v.max(0.0)))
    }

    /// The vr network of ndsr-native.md section 7.3 on `x[k][n]` (k along x) against the canonical layout on the
    /// transposed patch, with the blob repacked by [`from_vr`] and the even layers contracted with W2 first: the same
    /// `Y` bit for bit, every element. (With W1 first in every layer the roundings differ, so the kernel does not.)
    #[test]
    fn repacked_blob_gives_the_same_network() {
        repacked(LAYERS);
    }

    /// The same for the 3-layer blob: an odd number of layers leaves the vr result transposed.
    #[test]
    fn repacked_short_blob_gives_the_same_network() {
        repacked(3);
    }

    fn repacked(layers: usize) {
        // values k / 512 with |k| <= 128: products and 16-term sums are exact in f64, and rounding to f16 is exercised
        let mut state = 0x1234_5678_u32;
        let mut next = move || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            f64::from(i32::try_from(state >> 24).unwrap_or(0) - 128) / 512.0
        };
        // the blob is stored as f16 bit patterns; keep the values in a side table keyed by the pattern index
        let len = layers * LAYER_BYTES;
        let vals: Vec<f64> = (0..len / 2).map(|_| next()).collect();
        let bytes: Vec<u8> = (0..len / 2)
            .flat_map(|i| u16::try_from(i & 0xffff).unwrap_or(0).to_le_bytes())
            .collect();
        let canon = from_vr(&bytes).expect("packs");
        let value_vr = |i: usize| vals[i];
        let value_canon = |i: usize| vals[half_at(&canon, i)];
        let x: [[f64; 16]; 16] = {
            let mut m = [[0.0; 16]; 16];
            for row in &mut m {
                for v in row.iter_mut() {
                    *v = next();
                }
            }
            m
        };

        // vr form: X[k][n], Z = max(Y, 0)^T after each layer but the last
        let mut z = x;
        let mut y = x;
        for layer in 0..layers {
            let mut d: Option<[[f64; 16]; 16]> = None;
            for t in 0..2 {
                let s = 2 * layer + t;
                let w1 = mat(&value_vr, |r, k| vr_w1_index(s, r, k));
                let w2 = mat(&value_vr, |k, n| vr_w2_index(s, k, n));
                let h = hmma(&w1, &z, None);
                d = Some(hmma(&h, &w2, d.as_ref()));
            }
            y = d.expect("two terms");
            if layer < layers - 1 {
                z = transpose(&relu(&y));
            }
        }
        let y_vr = if layers % 2 == 1 { transpose(&y) } else { y };

        // canonical form: Z[row = y][col = x] = x^T
        let mut z = transpose(&x);
        let mut y = z;
        for layer in 0..layers {
            let mut d: Option<[[f64; 16]; 16]> = None;
            for t in 0..2 {
                let base = layer * LAYER_HALVES;
                let w1 = mat(&value_canon, |r, k| base + w1_index(t, r, k));
                let w2 = mat(&value_canon, |k, n| base + w2_index(t, k, n));
                d = Some(if layer % 2 == 1 {
                    let h = hmma(&w1, &z, None);
                    hmma(&h, &w2, d.as_ref())
                } else {
                    // the even layers contract with W2 first, as the vr programs do (the kernel's ndsr_layer_rev)
                    let h = hmma(&z, &w2, None);
                    hmma(&w1, &h, d.as_ref())
                });
            }
            y = d.expect("two terms");
            if layer < layers - 1 {
                z = relu(&y);
            }
        }
        for r in 0..16 {
            for c in 0..16 {
                assert_eq!(y[r][c].to_bits(), y_vr[r][c].to_bits(), "Y[{r}][{c}]");
            }
        }
        assert!(
            y.iter().flatten().any(|v| *v != 0.0),
            "the test network is not all zero"
        );
    }
}
