//! Vertex pulling: the `_bp` library of a vertex shader reads its inputs from the streams itself.
//!
//! The translator writes `<key>_bp.metallib` for every vertex shader whose inputs it can pull (`pull` field of
//! `_b.json`, `nxb-xlate` `msl/variant/pull.rs`). Its entry takes a `constant dm_vlayout&` at buffer [`SLOT`] and
//! decodes every attribute with `dm_fetch` (`dm_fetch.metal`). The host fills that block per draw ([`layout`]) and
//! binds it instead of the vertex streams and the default vertex. A pipeline whose vertex function pulls has no vertex
//! layout, so one unspecialized base per shader pair serves every layout the game draws with, and the build's archive
//! holds it.
//!
//! Pulling is off by default until it is verified in a game (Ocarina of Time's title stalled the GPU for up to 2 s with
//! it, see docs/diagnostics.md): `NXKIT_PULL=1` turns it on, otherwise every vertex shader draws with `_b` and the
//! stage_in vertex layout. `NXKIT_DIAG=pull` checks the reads of pulled draws against guest memory ([`model`]).

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::detail::DrawDetail;

pub(crate) mod model;

/// The buffer slot of the layout block (`pull::SLOT` of the translator, `[[buffer(16)]]` of the `_bp` entry).
pub(crate) const SLOT: u32 = 16;

/// The streams of the block: `dm_vstream s[16]`.
pub(crate) const STREAMS: usize = 16;

/// The bytes of one `dm_vstream`: base pointer 8, stride 4, divisor 4.
pub(crate) const STREAM_BYTES: usize = 16;

/// The byte offset of `uint attr[16]`.
pub(crate) const ATTR_OFFSET: usize = STREAMS * STREAM_BYTES;

/// The attribute words of the block, one per NVN attribute (the input location).
pub(crate) const ATTRIBUTES: usize = 16;

/// The bytes of `dm_vlayout`.
pub(crate) const BYTES: usize = ATTR_OFFSET + ATTRIBUTES * 4;

/// One layout block, as `dm_fetch` reads it.
pub(crate) type Block = [u8; BYTES];

/// Whether the host draws vertex shaders through their `_bp` library: only with `NXKIT_PULL=1`. Read once.
pub(crate) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| enabled(std::env::var("NXKIT_PULL").ok().as_deref()))
}

/// The value of `NXKIT_PULL` that turns pulling on: `1`. Unset, `0` or anything else keeps stage_in.
pub(crate) fn enabled(v: Option<&str>) -> bool {
    v == Some("1")
}

/// The attribute word of `dm_fetch`: the NVN attribute state word, format in bits 0 to 7, offset in 8 to 26, stream
/// in 27 to 31. 0 (format 0) reads zeros, which is what an input without an attribute gets: the same value as the
/// zero default vertex of the stage_in path. A value that does not fit gives 0 too.
pub(crate) fn attribute_word(format: u32, offset: u32, stream: u32) -> u32 {
    if format > 0xff || offset > 0x7_ffff || stream as usize >= STREAMS {
        return 0;
    }
    format | offset << 8 | stream << 27
}

/// The `dm_vlayout` block of a draw.
///
/// Per stream: the real GPU address of its buffer rounded down to 4 bytes, the stride (0: every vertex reads element 0)
/// and the divisor (0: per vertex). The low bits of the address move into the offsets of the stream's attributes, as
/// `dm_fetch` expects. Per input location: the attribute word, or 0 when the location has no attribute or its stream
/// has no buffer. The base vertex and base instance are not in the block: the vertex function gets them through
/// `[[vertex_id]]`, `[[instance_id]]` and `[[base_instance]]`, also for indirect draws.
///
/// `real` gives the real address of a guest GPU address, or `None` when it lies in no pool. The block holds real
/// addresses because `dm_fetch` dereferences them. The low bits that move into the offsets are those of the real
/// address, which the GPU sees. A stream with no real address is left out, so its attributes read the default.
pub(crate) fn layout(d: &DrawDetail, real: impl Fn(u64) -> Option<u64>) -> Block {
    let mut b = [0u8; BYTES];
    let mut low = [None::<u32>; STREAMS];
    for s in &d.streams {
        let i = s.index as usize;
        if i >= STREAMS {
            continue;
        }
        let Some(address) = real(s.address) else {
            continue;
        };
        let at = i * STREAM_BYTES;
        b[at..at + 8].copy_from_slice(&(address & !3).to_le_bytes());
        b[at + 8..at + 12].copy_from_slice(&s.stride.to_le_bytes());
        b[at + 12..at + 16].copy_from_slice(&s.divisor.to_le_bytes());
        low[i] = Some((address & 3) as u32);
    }
    for a in &d.attributes {
        let l = a.location as usize;
        if l >= ATTRIBUTES {
            continue;
        }
        let w = low
            .get(a.stream as usize)
            .copied()
            .flatten()
            .map_or(0, |low| {
                attribute_word(a.format, a.offset.saturating_add(low), a.stream)
            });
        let at = ATTR_OFFSET + l * 4;
        b[at..at + 4].copy_from_slice(&w.to_le_bytes());
    }
    b
}

/// Draws prepared with pulled vertices and with the stage_in layout, since the last [`take`].
static PULLED: AtomicU64 = AtomicU64::new(0);
static STAGE_IN: AtomicU64 = AtomicU64::new(0);

/// Counts a prepared vertex draw for the `pipe` line.
pub(crate) fn count(pulled: bool) {
    if pulled { &PULLED } else { &STAGE_IN }.fetch_add(1, Ordering::Relaxed);
}

/// The draws pulled and drawn with stage_in since the last call.
pub(crate) fn take() -> (u64, u64) {
    (
        PULLED.swap(0, Ordering::Relaxed),
        STAGE_IN.swap(0, Ordering::Relaxed),
    )
}

/// Pulled reads outside guest memory that `NXKIT_DIAG=pull` found, in total.
static STRAYS: AtomicU64 = AtomicU64::new(0);

/// `NXKIT_DIAG=pull`: checks that every element a pulled draw reads lies in guest memory, for the draws whose vertex
/// range the host knows (arrays, and indexed draws with readable indices), and prints the first reads that do not. A
/// read outside makes the GPU fault; the command buffer then fails (`nxkit-metal: warning: frame ... failed`). The
/// check works on guest addresses, so it builds its own block with the guest bases of the streams.
pub(crate) fn diag_draw(sh: &crate::draws::Shared<'_>, d: &DrawDetail, inputs: &[u32]) {
    let block = &layout(d, Some);
    let indices = |index_type: u32, address: u64, count: u32| {
        let size = 1u64 << index_type.min(2);
        sh.guest_offset(address, u64::from(count) * size)?;
        let mut bytes = vec![0u8; (u64::from(count) * size) as usize];
        sh.read_guest(address, &mut bytes).then_some(())?;
        index_range(index_type, &bytes)
    };
    let Some(ranges) = model::ranges(d, indices) else {
        return;
    };
    let inside = |a: u64, n: u64| sh.guest_offset(a, n).is_some();
    if let Some(s) = model::check(block, inputs, ranges, inside) {
        let n = STRAYS.fetch_add(1, Ordering::Relaxed) + 1;
        if n <= 32 {
            let m = sh.guest.address_map();
            eprintln!(
                "DIAG pull stray read {n}: vs {} location {} reads {} bytes at {:#x}, guest {:#x}+{:#x}, vertices {:?} \
                 instances {:?} args {:?} streams {:?} attributes {:?}",
                d.program.vertex,
                s.location,
                s.element.bytes,
                s.element.address,
                m.gpu_base(),
                m.len(),
                ranges.0,
                ranges.1,
                d.args,
                d.streams,
                d.attributes
            );
        }
    }
}

/// The lowest and highest index the GPU shades: 8-bit indices are widened to 16 bits, so 0xff is a vertex; 0xffff of
/// 16-bit and 0xffffffff of 32-bit indices restart the strip and shade nothing. `None` without a vertex.
pub(crate) fn index_range(index_type: u32, bytes: &[u8]) -> Option<(u32, u32)> {
    let it: Box<dyn Iterator<Item = u32>> = match index_type {
        0 => Box::new(bytes.iter().map(|&b| u32::from(b))),
        1 => Box::new(
            bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u32::from(u16::from_le_bytes(*c)))
                .filter(|&i| i != 0xffff),
        ),
        _ => Box::new(
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| u32::from_le_bytes(*c))
                .filter(|&i| i != u32::MAX),
        ),
    };
    it.fold(None, |r, i| match r {
        None => Some((i, i)),
        Some((lo, hi)) => Some((lo.min(i), hi.max(i))),
    })
}

#[cfg(test)]
mod tests;
