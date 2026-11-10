//! The copies the game records, followed back from the planes its NDVE dispatch binds to the memory the decoder's
//! pictures were read into.
//!
//! The game reads a picture into a buffer (`ReadDecoderOutputImageNv12`, a write of the host) and copies it into the
//! textures its colour conversion samples (`nvnCommandBufferCopyBufferToTexture`, or one more hop through a buffer to
//! buffer copy). The walk of every frame lists its copies with the GPU addresses of both sides
//! ([`CopyDetail::src_addr`], [`CopyDetail::dst_addr`]). [`Copies`] remembers the newest copy into each address, across
//! frames (the copy is recorded in the frame that reads the picture or in one before it), and [`Copies::chain`] gives
//! the memory a texture's contents came from, nearest hop first.

use std::collections::VecDeque;

use crate::detail::{CopyDetail, CopyKind};

/// The copies remembered. The game copies a few pictures' planes in flight.
const KEEP: usize = 64;

/// The most hops followed from a texture back to its origin.
const HOPS: usize = 4;

/// One copy: what it reads and what it writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Link {
    src: u64,
    dst: u64,
    /// The source is a texture's storage (laid out by the texture, not by the copy).
    src_texture: bool,
    row_stride: u64,
    /// The box of the copy in texels, for a copy with a texture side.
    size: [u32; 3],
}

/// Memory a texture was copied from: the GPU address and how the copy lays the rows out there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Hop {
    /// The GPU address of the source.
    pub(super) addr: u64,
    /// The bytes between two rows of the source, 0 for tightly packed. Only a buffer source has it.
    pub(super) row_stride: u64,
    /// The box of the copy in texels (width, height, depth), `[0; 3]` for a copy between buffers.
    pub(super) size: [u32; 3],
    /// True when the source is a texture's storage: its layout is the texture's, not a row stride.
    pub(super) texture: bool,
}

/// The newest copies, by destination.
#[derive(Debug, Default)]
pub(super) struct Copies {
    links: VecDeque<Link>,
}

impl Copies {
    /// Takes the copies of a frame, in recording order. A later copy into an address replaces an earlier one.
    pub(super) fn ingest(&mut self, copies: &[CopyDetail]) {
        for c in copies {
            let (src_texture, size) = match c.kind {
                CopyKind::BufferToTexture => (false, c.size),
                CopyKind::TextureToBuffer | CopyKind::TextureToTexture => (true, c.size),
                CopyKind::Other | CopyKind::BufferToBuffer => (false, [0; 3]),
                CopyKind::Clear | CopyKind::BufferFill => continue,
            };
            if c.src_addr == 0 || c.dst_addr == 0 {
                continue;
            }
            self.links.retain(|l| l.dst != c.dst_addr);
            self.links.push_back(Link {
                src: c.src_addr,
                dst: c.dst_addr,
                src_texture,
                row_stride: c.row_stride,
                size,
            });
            while self.links.len() > KEEP {
                self.links.pop_front();
            }
        }
    }

    /// The sources `addr` was copied from, the nearest first: the memory copied into `addr`, then the memory copied
    /// into that, and so on. Empty when nothing copied into `addr`.
    pub(super) fn chain(&self, addr: u64) -> Vec<Hop> {
        let mut out: Vec<Hop> = Vec::new();
        let mut at = addr;
        for _ in 0..HOPS {
            let Some(l) = self.links.iter().find(|l| l.dst == at) else {
                break;
            };
            if l.src == addr || out.iter().any(|h| h.addr == l.src) {
                break;
            }
            out.push(Hop {
                addr: l.src,
                row_stride: l.row_stride,
                size: l.size,
                texture: l.src_texture,
            });
            at = l.src;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b2t(buffer: u64, texture: u64) -> CopyDetail {
        CopyDetail {
            kind: CopyKind::BufferToTexture,
            src_addr: buffer,
            dst_addr: texture,
            size: [1920, 1080, 1],
            row_stride: 0,
            ..CopyDetail::default()
        }
    }

    fn b2b(src: u64, dst: u64) -> CopyDetail {
        CopyDetail {
            src_addr: src,
            dst_addr: dst,
            ..CopyDetail::default()
        }
    }

    #[test]
    fn a_texture_resolves_to_the_buffer_it_was_copied_from() {
        let mut c = Copies::default();
        c.ingest(&[b2t(0x1000, 0x9000)]);
        let chain = c.chain(0x9000);
        assert_eq!(chain.len(), 1);
        assert_eq!((chain[0].addr, chain[0].size), (0x1000, [1920, 1080, 1]));
        assert!(!chain[0].texture);
        assert!(c.chain(0x1000).is_empty());
    }

    #[test]
    fn a_chain_of_copies_is_followed_nearest_hop_first() {
        let mut c = Copies::default();
        // The copy that fills the staging buffer is recorded in an earlier frame than the one into the texture.
        c.ingest(&[b2b(0x1000, 0x5000)]);
        c.ingest(&[b2t(0x5000, 0x9000)]);
        let addrs: Vec<u64> = c.chain(0x9000).iter().map(|h| h.addr).collect();
        assert_eq!(addrs, [0x5000, 0x1000]);
    }

    #[test]
    fn the_newest_copy_into_an_address_wins() {
        let mut c = Copies::default();
        c.ingest(&[b2t(0x1000, 0x9000), b2t(0x2000, 0x9000)]);
        let addrs: Vec<u64> = c.chain(0x9000).iter().map(|h| h.addr).collect();
        assert_eq!(addrs, [0x2000]);
    }

    #[test]
    fn a_loop_and_a_long_chain_end() {
        let mut c = Copies::default();
        c.ingest(&[b2b(0x2000, 0x1000), b2b(0x1000, 0x2000)]);
        assert_eq!(c.chain(0x1000).len(), 1);
        let mut long = Copies::default();
        let copies: Vec<CopyDetail> = (0..10u64)
            .map(|i| b2b(0x1000 * (i + 2), 0x1000 * (i + 1)))
            .collect();
        long.ingest(&copies);
        assert_eq!(long.chain(0x1000).len(), HOPS);
    }

    #[test]
    fn copies_without_addresses_and_clears_are_ignored_and_old_ones_are_forgotten() {
        let mut c = Copies::default();
        c.ingest(&[
            CopyDetail::default(),
            CopyDetail {
                kind: CopyKind::Clear,
                dst_addr: 0x9000,
                src_addr: 1,
                ..CopyDetail::default()
            },
        ]);
        assert!(c.chain(0x9000).is_empty());
        let many: Vec<CopyDetail> = (0..KEEP as u64 + 5)
            .map(|i| b2t(0x100 + i, 0x10_000 + i))
            .collect();
        c.ingest(&many);
        assert!(c.chain(0x10_000).is_empty());
        assert_eq!(c.chain(0x10_000 + KEEP as u64 + 4).len(), 1);
    }

    #[test]
    fn a_texture_source_is_marked() {
        let mut c = Copies::default();
        c.ingest(&[CopyDetail {
            kind: CopyKind::TextureToTexture,
            src_addr: 0x4000,
            dst_addr: 0x8000,
            size: [64, 64, 1],
            ..CopyDetail::default()
        }]);
        assert!(c.chain(0x8000)[0].texture);
    }
}
