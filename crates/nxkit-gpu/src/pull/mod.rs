//! Vertex pulling: the `_bp` library of a vertex shader reads its inputs from the streams itself.
//!
//! The translator writes `<key>_bp.metallib` for every vertex shader whose inputs it can pull (`pull` field of
//! `_b.json`, `nxb-xlate` `msl/variant/pull.rs`). Its entry takes a `constant dm_vlayout&` at buffer [`SLOT`] and
//! decodes every attribute with `dm_fetch` (`dm_fetch.metal`). The host fills that block per draw ([`layout`]) and
//! binds it instead of the vertex streams and the default vertex. A pipeline whose vertex function pulls has no vertex
//! layout, so one unspecialized base per shader pair serves every layout the game draws with, and the build's archive
//! holds it.
//!
//! `NXKIT_PULL=0` draws with `_b` and the stage_in vertex layout as before.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::detail::DrawDetail;

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

/// Whether the host draws vertex shaders through their `_bp` library: on unless `NXKIT_PULL=0`. Read once.
pub(crate) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("NXKIT_PULL").map_or(true, |v| v != "0"))
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
/// Per stream: the GPU address of its buffer rounded down to 4 bytes, the stride (0: every vertex reads element 0)
/// and the divisor (0: per vertex). The low bits of the address move into the offsets of the stream's attributes, as
/// `dm_fetch` expects. Per input location: the attribute word, or 0 when the location has no attribute or its stream
/// has no buffer. The base vertex and base instance are not in the block: the vertex function gets them through
/// `[[vertex_id]]`, `[[instance_id]]` and `[[base_instance]]`, also for indirect draws.
pub(crate) fn layout(d: &DrawDetail) -> Block {
    let mut b = [0u8; BYTES];
    let mut low = [None::<u32>; STREAMS];
    for s in &d.streams {
        let i = s.index as usize;
        if i >= STREAMS {
            continue;
        }
        let at = i * STREAM_BYTES;
        b[at..at + 8].copy_from_slice(&(s.address & !3).to_le_bytes());
        b[at + 8..at + 12].copy_from_slice(&s.stride.to_le_bytes());
        b[at + 12..at + 16].copy_from_slice(&s.divisor.to_le_bytes());
        low[i] = Some((s.address & 3) as u32);
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

#[cfg(test)]
mod tests;
