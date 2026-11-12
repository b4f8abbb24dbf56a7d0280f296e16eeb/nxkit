//! AES-128 encryption with the ARMv8 Crypto Extension (`AESE`/`AESMC`), for CTR over whole blocks (the NCA bulk path
//! of nxb-extract). Detected at run time, callers fall back to a table cipher. Moved here from nxb-extract (X7).

/// Whether the CPU has the AES instructions.
pub fn available() -> bool {
    #[cfg(target_arch = "aarch64")]
    {
        std::arch::is_aarch64_feature_detected!("aes")
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        false
    }
}

/// CTR over whole blocks: `data` starts at counter block `block` (low 8 bytes, big-endian), upper 8 bytes `upper`.
/// `rk` are the 11 round keys as bytes. Caller checks [`available`].
#[cfg(target_arch = "aarch64")]
pub fn ctr_blocks(rk: &[[u8; 16]; 11], upper: [u8; 8], block: u64, data: &mut [u8]) {
    if !available() {
        return;
    }
    // SAFETY: the AES feature was detected above, all loads and stores are on 16-byte arrays.
    unsafe { ctr_blocks_hw(rk, upper, block, data) }
}

/// Off AArch64 [`available`] is false, so callers take the table cipher and never reach this.
#[cfg(not(target_arch = "aarch64"))]
pub fn ctr_blocks(_rk: &[[u8; 16]; 11], _upper: [u8; 8], _block: u64, _data: &mut [u8]) {}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "aes,neon")]
unsafe fn ctr_blocks_hw(rk: &[[u8; 16]; 11], upper: [u8; 8], mut block: u64, data: &mut [u8]) {
    use std::arch::aarch64::*;
    // SAFETY: neon and aes are enabled for this function, every pointer comes from a 16-byte array.
    unsafe {
        let k: [uint8x16_t; 11] = std::array::from_fn(|i| vld1q_u8(rk[i].as_ptr()));
        let ctr = |b: u64| {
            let mut c = [0u8; 16];
            c[..8].copy_from_slice(&upper);
            c[8..].copy_from_slice(&b.to_be_bytes());
            vld1q_u8(c.as_ptr())
        };
        let enc = |mut s: uint8x16_t| {
            for kr in &k[..9] {
                s = vaesmcq_u8(vaeseq_u8(s, *kr));
            }
            veorq_u8(vaeseq_u8(s, k[9]), k[10])
        };
        let (chunks, rest) = data.as_chunks_mut::<128>();
        for c in chunks {
            let mut s: [uint8x16_t; 8] = std::array::from_fn(|i| ctr(block.wrapping_add(i as u64)));
            for kr in &k[..9] {
                for x in s.iter_mut() {
                    *x = vaesmcq_u8(vaeseq_u8(*x, *kr));
                }
            }
            for (i, x) in s.iter().enumerate() {
                let ks = veorq_u8(vaeseq_u8(*x, k[9]), k[10]);
                let p = c.as_mut_ptr().add(16 * i);
                vst1q_u8(p, veorq_u8(vld1q_u8(p), ks));
            }
            block = block.wrapping_add(8);
        }
        for b in rest.as_chunks_mut::<16>().0 {
            let ks = enc(ctr(block));
            let p = b.as_mut_ptr();
            vst1q_u8(p, veorq_u8(vld1q_u8(p), ks));
            block = block.wrapping_add(1);
        }
    }
}
