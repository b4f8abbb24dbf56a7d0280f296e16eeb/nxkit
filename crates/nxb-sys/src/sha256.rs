//! Hardware SHA-256 block compression (ARMv8 SHA2 instructions), detected at run time.
//!
//! Output-neutral: it computes exactly the FIPS 180-4 compression function, so it never changes a digest.`nxb-model`
//! checks it against the same vectors as its scalar path (decisions.md W17). Callers fall back to their own
//! scalar code when [`compress_blocks`] returns `false`.

/// True when this CPU has the SHA2 instructions (checked once).
pub fn available() -> bool {
    imp::available()
}

/// Compress whole 64-byte blocks into `state` with the hardware path. Returns `false` (and leaves `state` untouched)
/// when the CPU or the build target has no SHA2 instructions.
pub fn compress_blocks(state: &mut [u32; 8], blocks: &[[u8; 64]]) -> bool {
    imp::compress_blocks(state, blocks)
}

#[cfg(target_arch = "aarch64")]
mod imp {
    use std::arch::aarch64::*;
    use std::sync::OnceLock;

    static AVAILABLE: OnceLock<bool> = OnceLock::new();

    pub(super) fn available() -> bool {
        *AVAILABLE.get_or_init(|| std::arch::is_aarch64_feature_detected!("sha2"))
    }

    pub(super) fn compress_blocks(state: &mut [u32; 8], blocks: &[[u8; 64]]) -> bool {
        if !available() {
            return false;
        }
        // SAFETY: the SHA2 feature (which implies NEON) was detected on this CPU just above.
        unsafe { compress_hw(state, blocks) };
        true
    }

    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    /// # Safety
    /// The CPU must support the `sha2` target feature.
    #[target_feature(enable = "sha2")]
    unsafe fn compress_hw(state: &mut [u32; 8], blocks: &[[u8; 64]]) {
        // SAFETY: every pointer comes from a live array of the right length (state: 8 words, block: 64 bytes,
        // K: 64 words read 4 at a time at offsets 0..60), NEON loads and stores have no alignment needs.
        unsafe {
            let mut abcd = vld1q_u32(state.as_ptr());
            let mut efgh = vld1q_u32(state.as_ptr().add(4));
            for block in blocks {
                let (abcd0, efgh0) = (abcd, efgh);
                let p = block.as_ptr();
                let mut m = [
                    vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(p))),
                    vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(p.add(16)))),
                    vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(p.add(32)))),
                    vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(p.add(48)))),
                ];
                for i in 0..16 {
                    let t = vaddq_u32(m[i % 4], vld1q_u32(K.as_ptr().add(4 * i)));
                    let prev = abcd;
                    abcd = vsha256hq_u32(abcd, efgh, t);
                    efgh = vsha256h2q_u32(efgh, prev, t);
                    if i < 12 {
                        // W[4i+16..4i+20] from W[4i..4i+16]
                        m[i % 4] = vsha256su1q_u32(
                            vsha256su0q_u32(m[i % 4], m[(i + 1) % 4]),
                            m[(i + 2) % 4],
                            m[(i + 3) % 4],
                        );
                    }
                }
                abcd = vaddq_u32(abcd, abcd0);
                efgh = vaddq_u32(efgh, efgh0);
            }
            vst1q_u32(state.as_mut_ptr(), abcd);
            vst1q_u32(state.as_mut_ptr().add(4), efgh);
        }
    }
}

#[cfg(not(target_arch = "aarch64"))]
mod imp {
    // x86 SHA-NI is a possible later addition, until then the caller's scalar path runs.
    pub(super) fn available() -> bool {
        false
    }

    pub(super) fn compress_blocks(_state: &mut [u32; 8], _blocks: &[[u8; 64]]) -> bool {
        false
    }
}

#[cfg(all(test, target_arch = "aarch64"))]
mod tests {
    use super::*;

    /// One block of "abc" padded: the FIPS example. Full digest tests live in nxb-model.
    #[test]
    fn abc_block() {
        if !available() {
            return;
        }
        let mut block = [0u8; 64];
        block[..3].copy_from_slice(b"abc");
        block[3] = 0x80;
        block[63] = 24;
        let mut st = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
            0x5be0cd19,
        ];
        assert!(compress_blocks(&mut st, &[block]));
        assert_eq!(st[0], 0xba7816bf);
        assert_eq!(st[7], 0xf20015ad);
    }
}
