//! SHA-256 (FIPS 180-4), own implementation, std only.
//!
//! Two backends with identical output: the ARMv8 SHA2 instructions through `nxb-sys` when the CPU has them, else a
//! portable scalar implementation. Both are checked against the FIPS 180-2 example vectors and against Python's `hashlib`
//! on every length from 0 to 299 bytes (see the tests, decisions.md W3 and W17).

use crate::digest::Digest;

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// Incremental SHA-256 state.
///
/// ```
/// use nxb_model::{Sha256, sha256};
/// let mut h = Sha256::new();
/// h.update(b"ab");
/// h.update(b"c");
/// assert_eq!(h.finalize(), sha256(b"abc"));
/// ```
#[derive(Clone)]
pub struct Sha256 {
    backend: Backend,
    state: [u32; 8],
    block: [u8; 64],
    block_len: usize,
    total_len: u64,
}

/// Which compression function a [`Sha256`] uses. Digests are identical; only speed differs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Backend {
    /// The hardware path when the CPU has it, else scalar.
    Auto,
    /// Always the portable scalar code (tests, comparisons).
    Scalar,
}

impl Backend {
    /// What [`Backend::Auto`] resolves to on this machine: `armv8-sha2` or `scalar` (for metrics and logs).
    pub fn detected_name() -> &'static str {
        if nxb_sys::sha256::available() {
            "armv8-sha2"
        } else {
            "scalar"
        }
    }
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Sha256 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sha256")
            .field("total_len", &self.total_len)
            .finish_non_exhaustive()
    }
}

impl Sha256 {
    /// A fresh hasher.
    pub fn new() -> Self {
        Self::with_backend(Backend::Auto)
    }

    /// A fresh hasher with a chosen backend.
    pub fn with_backend(backend: Backend) -> Self {
        Sha256 {
            backend,
            state: H0,
            block: [0; 64],
            block_len: 0,
            total_len: 0,
        }
    }

    /// Number of bytes fed so far.
    pub fn len(&self) -> u64 {
        self.total_len
    }

    /// True when no byte was fed yet.
    pub fn is_empty(&self) -> bool {
        self.total_len == 0
    }

    /// Feed bytes.
    pub fn update(&mut self, mut data: &[u8]) {
        self.total_len = self.total_len.wrapping_add(data.len() as u64);
        if self.block_len > 0 {
            let take = (64 - self.block_len).min(data.len());
            self.block[self.block_len..self.block_len + take].copy_from_slice(&data[..take]);
            self.block_len += take;
            data = &data[take..];
            if self.block_len < 64 {
                return;
            }
            let block = self.block;
            self.compress_blocks(&[block]);
            self.block_len = 0;
        }
        let (blocks, rest) = data.as_chunks::<64>();
        self.compress_blocks(blocks);
        self.block[..rest.len()].copy_from_slice(rest);
        self.block_len = rest.len();
    }

    /// Finish and return the digest.
    pub fn finalize(mut self) -> Digest {
        let bit_len = self.total_len.wrapping_mul(8);
        let mut pad = [0u8; 72];
        pad[0] = 0x80;
        // Pad to 56 mod 64, then the 64-bit big-endian length.
        let pad_len = if self.block_len < 56 {
            56 - self.block_len
        } else {
            120 - self.block_len
        };
        let saved_total = self.total_len;
        self.update(&pad[..pad_len]);
        self.update(&bit_len.to_be_bytes());
        self.total_len = saved_total;
        let mut out = [0u8; 32];
        for (dst, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(self.state.iter()) {
            *dst = word.to_be_bytes();
        }
        Digest(out)
    }
}

impl Sha256 {
    fn compress_blocks(&mut self, blocks: &[[u8; 64]]) {
        if blocks.is_empty() {
            return;
        }
        if self.backend == Backend::Auto
            && nxb_sys::sha256::compress_blocks(&mut self.state, blocks)
        {
            return;
        }
        for block in blocks {
            compress(&mut self.state, block);
        }
    }
}

fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for (i, word) in block.as_chunks::<4>().0.iter().enumerate() {
        w[i] = u32::from_be_bytes(*word);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ (!e & g);
        let t1 = h
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    for (s, v) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *s = s.wrapping_add(v);
    }
}

/// SHA-256 of a byte slice.
pub fn sha256(data: &[u8]) -> Digest {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxb_testkit::Rng;

    fn hex(d: Digest) -> String {
        d.to_hex()
    }

    #[test]
    fn fips_vectors() {
        assert_eq!(
            hex(sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(sha256(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            hex(sha256(b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu")),
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1"
        );
    }

    #[test]
    fn million_a() {
        let mut h = Sha256::new();
        let chunk = [b'a'; 1000];
        for _ in 0..1000 {
            h.update(&chunk);
        }
        assert_eq!(
            hex(h.finalize()),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    /// Every length 0..300 (all padding branches) against Python `hashlib`: the digest of the concatenated digests of
    /// `bytes((i*7+3) & 0xff for i in range(n))`.
    fn with(b: Backend, data: &[u8]) -> Digest {
        let mut h = Sha256::with_backend(b);
        h.update(data);
        h.finalize()
    }

    #[test]
    fn all_lengths_against_hashlib_both_backends() {
        for b in [Backend::Auto, Backend::Scalar] {
            let mut acc = Vec::new();
            for n in 0..300usize {
                let data: Vec<u8> = (0..n).map(|i| ((i * 7 + 3) & 0xff) as u8).collect();
                acc.extend_from_slice(&with(b, &data).0);
            }
            assert_eq!(
                hex(with(b, &acc)),
                "7b074096cabb18dd0d1b468a173cb2f97f80e952525bca29542e606fd6d0753a",
                "{b:?}"
            );
        }
    }

    #[test]
    fn fips_vectors_scalar() {
        assert_eq!(
            hex(with(Backend::Scalar, b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let a = vec![b'a'; 1_000_000];
        assert_eq!(
            hex(with(Backend::Scalar, &a)),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    /// Property: the backends agree on random inputs and random chunkings.
    #[test]
    fn backends_agree() {
        nxb_testkit::check(0x5eed_0002, 100, |r| {
            let n = r.below(5000) as usize;
            let data = r.bytes(n);
            let mut h = Sha256::with_backend(Backend::Auto);
            for part in data.chunks(1 + r.below(200) as usize) {
                h.update(part);
            }
            assert_eq!(h.finalize(), with(Backend::Scalar, &data));
        });
    }

    /// Property: any split of the input into update calls gives the one-shot digest.
    #[test]
    fn chunking_is_irrelevant() {
        let mut rng = Rng::new(0x5eed_0001);
        for _ in 0..200 {
            let len = rng.below(1000) as usize;
            let data = rng.bytes(len);
            let whole = sha256(&data);
            let mut h = Sha256::new();
            let mut rest = &data[..];
            while !rest.is_empty() {
                let take = (rng.below(130) as usize).min(rest.len());
                h.update(&rest[..take]);
                rest = &rest[take..];
            }
            assert_eq!(h.len(), len as u64);
            assert_eq!(h.finalize(), whole);
        }
    }
}

/// Throughput probe, not a gate: `cargo test --release -p nxb-model -- --ignored --nocapture sha256_throughput`.
#[cfg(test)]
mod throughput {
    use super::{Backend, Sha256};

    #[test]
    #[ignore = "timing probe; run in release"]
    fn sha256_throughput() {
        let data = vec![0xa5u8; 1 << 30];
        let mut digests = Vec::new();
        for b in [Backend::Auto, Backend::Scalar] {
            let t = std::time::Instant::now();
            let mut h = Sha256::with_backend(b);
            h.update(&data);
            digests.push(h.finalize());
            let s = t.elapsed().as_secs_f64();
            eprintln!(
                "sha256 {b:?} ({}): 1 GiB in {s:.2} s = {:.0} MB/s",
                Backend::detected_name(),
                1073.741824 / s
            );
        }
        assert_eq!(digests[0], digests[1]);
    }
}
