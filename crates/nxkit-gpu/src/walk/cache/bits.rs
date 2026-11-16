//! The bit sets of the fields that a recording reads and writes, and what each kind of record reads.

use crate::walk::{ATTRIBUTES, BANKS, STREAMS, TEXTURE_SLOTS};

pub(super) const WORDS: usize = 7;

/// A set of fields of the walker state. One bit is one slot or one group of values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::walk) struct Bits(pub(super) [u64; WORDS]);

impl Bits {
    pub(super) const NONE: Bits = Bits([0; WORDS]);

    pub(super) const fn range(start: usize, len: usize) -> Bits {
        let mut w = [0u64; WORDS];
        let mut i = start;
        while i < start + len {
            w[i / 64] |= 1 << (i % 64);
            i += 1;
        }
        Bits(w)
    }

    const fn or(self, o: Bits) -> Bits {
        let mut w = self.0;
        let mut i = 0;
        while i < WORDS {
            w[i] |= o.0[i];
            i += 1;
        }
        Bits(w)
    }

    /// The scalar groups of `list` (indices of [`sc`]).
    const fn scalars(list: &[usize]) -> Bits {
        let mut b = Bits::NONE;
        let mut i = 0;
        while i < list.len() {
            b = b.or(Bits::range(SC + list[i], 1));
            i += 1;
        }
        b
    }

    pub(super) fn has(&self, bit: usize) -> bool {
        self.0
            .get(bit / 64)
            .is_some_and(|w| w >> (bit % 64) & 1 != 0)
    }

    pub(super) fn set(&mut self, bit: usize) {
        if let Some(w) = self.0.get_mut(bit / 64) {
            *w |= 1 << (bit % 64);
        }
    }

    /// Adds the bits of `m` that `except` does not have.
    pub(super) fn add_missing(&mut self, m: &Bits, except: &Bits) {
        for ((w, m), e) in self.0.iter_mut().zip(&m.0).zip(&except.0) {
            *w |= m & !e;
        }
    }

    pub(super) fn add(&mut self, m: &Bits) {
        for (w, m) in self.0.iter_mut().zip(&m.0) {
            *w |= m;
        }
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.0.iter().enumerate().flat_map(|(w, bits)| {
            let mut rest = *bits;
            std::iter::from_fn(move || {
                if rest == 0 {
                    return None;
                }
                let at = rest.trailing_zeros() as usize;
                rest &= rest - 1;
                Some(w * 64 + at)
            })
        })
    }
}

/// The first bit of each kind of field.
pub(super) const P: usize = 0;
pub(super) const VB: usize = P + 6;
pub(super) const UBO: usize = VB + STREAMS;
pub(super) const SBO: usize = UBO + 16 * BANKS;
pub(super) const ATTR: usize = SBO + 16 * BANKS;
pub(super) const STREAM: usize = ATTR + ATTRIBUTES;
pub(super) const BLEND: usize = STREAM + STREAMS;
pub(super) const TEX: usize = BLEND + 8;
pub(super) const SC: usize = TEX + TEXTURE_SLOTS;

/// The groups of scalar fields, as an index from [`SC`].
pub(in crate::walk) mod sc {
    pub(in crate::walk) const BLEND_ENABLE: usize = 0;
    pub(in crate::walk) const MASKS: usize = 1;
    pub(in crate::walk) const DEPTH_STENCIL: usize = 2;
    pub(in crate::walk) const STENCIL_READ: usize = 3;
    pub(in crate::walk) const STENCIL_WRITE: usize = 5;
    pub(in crate::walk) const STENCIL_REF: usize = 7;
    pub(in crate::walk) const POLYGON: usize = 9;
    pub(in crate::walk) const MULTISAMPLE: usize = 10;
    pub(in crate::walk) const OFFSET: usize = 11;
    pub(in crate::walk) const CLAMP: usize = 12;
    pub(in crate::walk) const VIEWPORT: usize = 13;
    pub(in crate::walk) const DEPTH_RANGE: usize = 14;
    pub(in crate::walk) const SCISSOR: usize = 15;
    pub(in crate::walk) const BLEND_COLOR: usize = 16;
    pub(in crate::walk) const STRIDE_ROW: usize = 17;
    pub(in crate::walk) const STRIDE_IMAGE: usize = 18;
    pub(in crate::walk) const VIEWPORT_SWIZZLE: usize = 19;
    pub(in crate::walk) const ORIGINS: usize = 20;
}

/// The bit of the program slot `stage`.
pub(in crate::walk) const fn program_bit(stage: usize) -> usize {
    P + stage
}

/// The bit of the vertex buffer `stream`.
pub(in crate::walk) const fn vertex_buffer_bit(stream: usize) -> usize {
    VB + stream
}

/// The bit of the vertex stream `stream` (stride and divisor).
pub(in crate::walk) const fn stream_bit(stream: usize) -> usize {
    STREAM + stream
}

/// The bit of the vertex attribute `location`.
pub(in crate::walk) const fn attribute_bit(location: usize) -> usize {
    ATTR + location
}

/// The bit of the uniform buffer `slot` of the bank (0 vertex, 1 fragment, 2 compute, 3 task, 4 mesh).
pub(in crate::walk) const fn ubo_bit(bank: usize, slot: usize) -> usize {
    UBO + bank * 16 + slot
}

/// The bit of the storage buffer `slot` of the bank.
pub(in crate::walk) const fn sbo_bit(bank: usize, slot: usize) -> usize {
    SBO + bank * 16 + slot
}

/// The bit of the blend object of `target`.
pub(in crate::walk) const fn blend_bit(target: usize) -> usize {
    BLEND + target
}

/// The bit of the texture handle in `slot`.
pub(in crate::walk) const fn texture_bit(slot: usize) -> usize {
    TEX + slot
}

/// The bit of a scalar group.
pub(in crate::walk) const fn scalar_bit(group: usize) -> usize {
    SC + group
}

/// What a draw reads of the state, apart from the programs and the vertex buffers of its streams.
pub(in crate::walk) const DRAW_READS: Bits = Bits::range(BLEND, 8)
    .or(Bits::range(ATTR, ATTRIBUTES))
    .or(Bits::range(UBO, 32))
    .or(Bits::range(SBO, 32))
    .or(Bits::range(TEX, 64))
    .or(Bits::scalars(&[
        sc::BLEND_ENABLE,
        sc::MASKS,
        sc::DEPTH_STENCIL,
        sc::STENCIL_READ,
        sc::STENCIL_READ + 1,
        sc::STENCIL_WRITE,
        sc::STENCIL_WRITE + 1,
        sc::STENCIL_REF,
        sc::STENCIL_REF + 1,
        sc::POLYGON,
        sc::MULTISAMPLE,
        sc::OFFSET,
        sc::CLAMP,
        sc::VIEWPORT,
        sc::VIEWPORT_SWIZZLE,
        sc::ORIGINS,
        sc::DEPTH_RANGE,
        sc::SCISSOR,
        sc::BLEND_COLOR,
    ]));

/// What a mesh draw reads besides [`DRAW_READS`]: the buffers of the task and mesh banks and the texture bank that the
/// two stages share.
pub(in crate::walk) const MESH_READS: Bits = Bits::range(UBO + 48, 32)
    .or(Bits::range(SBO + 48, 32))
    .or(Bits::range(TEX + 96, 32));

/// What a dispatch reads of the state, apart from its program.
pub(in crate::walk) const DISPATCH_READS: Bits = Bits::range(UBO + 32, 16)
    .or(Bits::range(SBO + 32, 16))
    .or(Bits::range(TEX + 64, 32));

/// What a copy reads of the state.
pub(in crate::walk) const COPY_READS: Bits = Bits::scalars(&[sc::STRIDE_ROW, sc::STRIDE_IMAGE]);
