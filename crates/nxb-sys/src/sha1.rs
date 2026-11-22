//! Hardware SHA-1 block compression (ARMv8 SHA1 instructions), detected at run time. Output-neutral, like
//! [`crate::sha256`]. `nxb-model` tests it on the same vectors as its scalar path.

/// True when this CPU has the SHA1 instructions (part of the `sha2` crypto feature in Rust's naming).
pub fn available() -> bool {
    crate::sha256::available()
}

/// Compress whole blocks into `state`.`false` (state untouched) without hardware support.
pub fn compress_blocks(state: &mut [u32; 5], blocks: &[[u8; 64]]) -> bool {
    imp::compress_blocks(state, blocks)
}

#[cfg(target_arch = "aarch64")]
mod imp {
    use std::arch::aarch64::*;

    const K: [u32; 4] = [0x5a827999, 0x6ed9eba1, 0x8f1bbcdc, 0xca62c1d6];

    pub(super) fn compress_blocks(state: &mut [u32; 5], blocks: &[[u8; 64]]) -> bool {
        if !super::available() {
            return false;
        }
        // SAFETY: the SHA crypto feature (which implies NEON) was detected on this CPU just above.
        unsafe { compress_hw(state, blocks) };
        true
    }

    /// # Safety
    /// The CPU must support the `sha2` target feature (which carries the SHA1 instructions).
    #[target_feature(enable = "sha2")]
    unsafe fn compress_hw(state: &mut [u32; 5], blocks: &[[u8; 64]]) {
        // SAFETY: pointers come from live arrays of the right length (state: 5 words, the first 4 loaded and stored.
        // block: 64 bytes), NEON loads and stores have no alignment needs.
        unsafe {
            let mut abcd = vld1q_u32(state.as_ptr());
            let mut e0 = state[4];
            for block in blocks {
                let (abcd0, e_saved) = (abcd, e0);
                let p = block.as_ptr();
                let mut m = [
                    vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(p))),
                    vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(p.add(16)))),
                    vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(p.add(32)))),
                    vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(p.add(48)))),
                ];
                let mut e = e0;
                for i in 0..20 {
                    let wk = vaddq_u32(m[i % 4], vdupq_n_u32(K[i / 5]));
                    let e_next = vsha1h_u32(vgetq_lane_u32::<0>(abcd));
                    abcd = match i / 5 {
                        0 => vsha1cq_u32(abcd, e, wk),
                        2 => vsha1mq_u32(abcd, e, wk),
                        _ => vsha1pq_u32(abcd, e, wk),
                    };
                    e = e_next;
                    if i < 16 {
                        // W[4i+16..4i+20] from W[4i..4i+16]
                        m[i % 4] = vsha1su1q_u32(
                            vsha1su0q_u32(m[i % 4], m[(i + 1) % 4], m[(i + 2) % 4]),
                            m[(i + 3) % 4],
                        );
                    }
                }
                abcd = vaddq_u32(abcd, abcd0);
                e0 = e.wrapping_add(e_saved);
            }
            vst1q_u32(state.as_mut_ptr(), abcd);
            state[4] = e0;
        }
    }
}

#[cfg(not(target_arch = "aarch64"))]
mod imp {
    pub(super) fn compress_blocks(_state: &mut [u32; 5], _blocks: &[[u8; 64]]) -> bool {
        false
    }
}
