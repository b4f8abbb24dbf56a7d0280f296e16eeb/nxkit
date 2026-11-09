//! The addresses that `dm_fetch` reads, computed on the host, and the check `NXKIT_DIAG=pull` makes with them.
//!
//! `dm_fetch` (`dm_fetch.metal`) dereferences `base + rel` of the layout block as a `device` pointer: a real Metal GPU
//! address. The stream addresses of a draw are guest GPU addresses, which are virtual (they start at
//! `nxkit_metal::GUEST_VA_BASE`). The encoder resolves each stream start through the pool snapshot of the frame and
//! writes the real address into the layout block (`pull::layout`), the same value the stage_in path binds with
//! `setAddress`. So a pulled attribute reads the bytes that the stage_in fetch reads, as long as every element lies in
//! the buffer of the pool of its stream. A read outside it is a GPU address fault, which the command buffer reports as
//! an error and the GPU recovers from. [`check`] looks for such reads in the draws whose vertex range the host knows. It
//! works on guest addresses, so it only knows whether an element lies in guest memory, not in the pool of its stream.

use super::{ATTR_OFFSET, ATTRIBUTES, Block, STREAM_BYTES};
use crate::detail::{DrawArgs, DrawDetail};
use crate::format_map::vertex_format;
use nxkit_metal::VertexFormat;

/// (components, size code 0 / 1 / 2 for 8 / 16 / 32 bits, kind) of `dm_ftab` for a Metal vertex format. Kinds: 1 unorm,
/// 2 snorm, 3 uint, 4 sint, 5 float, 8 unorm 10:10:10:2, 9 snorm 10:10:10:2, 10 R11G11B10F, 11 BGRA8.
pub(crate) fn shape(f: VertexFormat) -> (u8, u8, u8) {
    use VertexFormat as V;
    match f {
        V::UCharNorm => (1, 0, 1),
        V::UChar2Norm => (2, 0, 1),
        V::UChar3Norm => (3, 0, 1),
        V::UChar4Norm => (4, 0, 1),
        V::CharNorm => (1, 0, 2),
        V::Char2Norm => (2, 0, 2),
        V::Char3Norm => (3, 0, 2),
        V::Char4Norm => (4, 0, 2),
        V::UChar => (1, 0, 3),
        V::UChar2 => (2, 0, 3),
        V::UChar3 => (3, 0, 3),
        V::UChar4 => (4, 0, 3),
        V::Char => (1, 0, 4),
        V::Char2 => (2, 0, 4),
        V::Char3 => (3, 0, 4),
        V::Char4 => (4, 0, 4),
        V::Half => (1, 1, 5),
        V::Half2 => (2, 1, 5),
        V::Half3 => (3, 1, 5),
        V::Half4 => (4, 1, 5),
        V::UShortNorm => (1, 1, 1),
        V::UShort2Norm => (2, 1, 1),
        V::UShort3Norm => (3, 1, 1),
        V::UShort4Norm => (4, 1, 1),
        V::ShortNorm => (1, 1, 2),
        V::Short2Norm => (2, 1, 2),
        V::Short3Norm => (3, 1, 2),
        V::Short4Norm => (4, 1, 2),
        V::UShort => (1, 1, 3),
        V::UShort2 => (2, 1, 3),
        V::UShort3 => (3, 1, 3),
        V::UShort4 => (4, 1, 3),
        V::Short => (1, 1, 4),
        V::Short2 => (2, 1, 4),
        V::Short3 => (3, 1, 4),
        V::Short4 => (4, 1, 4),
        V::Float => (1, 2, 5),
        V::Float2 => (2, 2, 5),
        V::Float3 => (3, 2, 5),
        V::Float4 => (4, 2, 5),
        V::UInt => (1, 2, 3),
        V::UInt2 => (2, 2, 3),
        V::UInt3 => (3, 2, 3),
        V::UInt4 => (4, 2, 3),
        V::Int => (1, 2, 4),
        V::Int2 => (2, 2, 4),
        V::Int3 => (3, 2, 4),
        V::Int4 => (4, 2, 4),
        V::UInt1010102Norm => (4, 2, 8),
        V::Int1010102Norm => (4, 2, 9),
        V::FloatRg11B10 => (3, 2, 10),
        V::UChar4NormBgra => (4, 0, 11),
    }
}

/// The bytes `dm_decode` loads for an element of `shape` whose address is (`aligned`) or is not a multiple of 4. A
/// whole-word load of an aligned 8- or 16-bit element can read past the element, never past 4 bytes or the packed pair.
pub(crate) fn read_bytes((n, sz, kind): (u8, u8, u8), aligned: bool) -> u32 {
    let n = u32::from(n);
    if (8..=10).contains(&kind) {
        4
    } else if sz == 2 {
        4 * n
    } else if aligned {
        if sz == 1 && n > 2 { 8 } else { 4 }
    } else if sz == 1 {
        2 * n
    } else {
        n
    }
}

/// One element that `dm_fetch` reads: its GPU address and the bytes it loads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Element {
    pub(crate) address: u64,
    pub(crate) bytes: u32,
}

fn word(b: &Block, at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// The element of input `location` that `dm_fetch(L, location, vid, iid, bi)` loads, with its 32-bit arithmetic:
/// `None` when the input reads zeros without a load (no attribute, or a format the table lacks).
pub(crate) fn element(b: &Block, location: u32, vid: u32, iid: u32, bi: u32) -> Option<Element> {
    if location as usize >= ATTRIBUTES {
        return None;
    }
    let w = word(b, ATTR_OFFSET + 4 * location as usize);
    let shape = shape(vertex_format(w & 0xff)?);
    let (off, st) = ((w >> 8) & 0x7_ffff, (w >> 27) as usize);
    let at = st * STREAM_BYTES;
    let mut base = [0u8; 8];
    base.copy_from_slice(&b[at..at + 8]);
    let (base, stride, div) = (u64::from_le_bytes(base), word(b, at + 8), word(b, at + 12));
    let idx = if stride == 0 {
        0
    } else {
        iid.wrapping_sub(bi)
            .checked_div(div)
            .map_or(vid, |i| i.wrapping_add(bi))
    };
    let rel = idx.wrapping_mul(stride).wrapping_add(off);
    Some(Element {
        address: base.wrapping_add(u64::from(rel)),
        bytes: read_bytes(shape, rel & 3 == 0),
    })
}

/// The vertex ids (`[[vertex_id]]`) and instance ids of a draw as inclusive ranges, when the host knows them without
/// the GPU: arrays draws, and indexed draws whose indices `indices` reads (index type, address, count).
pub(crate) fn ranges(
    d: &DrawDetail,
    indices: impl FnOnce(u32, u64, u32) -> Option<(u32, u32)>,
) -> Option<((u32, u32), (u32, u32))> {
    let inst = |instances: u32, base: u32| (base, base.wrapping_add(instances.max(1) - 1));
    match d.args {
        DrawArgs::Arrays {
            first,
            count,
            instances,
            base_instance,
        } if count > 0 => Some((
            (first, first.wrapping_add(count - 1)),
            inst(instances, base_instance),
        )),
        DrawArgs::Indexed {
            index_type,
            address,
            count,
            instances,
            base_vertex,
            base_instance,
        } if count > 0 => {
            let (lo, hi) = indices(index_type, address, count)?;
            let bv = base_vertex as u32;
            Some((
                (lo.wrapping_add(bv), hi.wrapping_add(bv)),
                inst(instances, base_instance),
            ))
        }
        _ => None,
    }
}

/// A pulled read outside guest memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Stray {
    pub(crate) location: u32,
    pub(crate) element: Element,
}

/// The first input of `locations` whose lowest or highest element over the vertex and instance ranges leaves
/// `inside(address, bytes)`. An element's address grows with the vertex and the instance, so the ends cover the range.
pub(crate) fn check(
    b: &Block,
    locations: &[u32],
    (vids, iids): ((u32, u32), (u32, u32)),
    inside: impl Fn(u64, u64) -> bool,
) -> Option<Stray> {
    let bi = iids.0;
    for &l in locations {
        for (vid, iid) in [(vids.0, iids.0), (vids.1, iids.1)] {
            if let Some(e) = element(b, l, vid, iid, bi)
                && !inside(e.address, u64::from(e.bytes))
            {
                return Some(Stray {
                    location: l,
                    element: e,
                });
            }
        }
    }
    None
}
