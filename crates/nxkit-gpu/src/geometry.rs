//! The geometry of a draw in Metal terms, and the address checks of its buffers.
//!
//! A draw names its index buffer, its indirect arguments and its vertex streams by guest GPU address. The GPU needs the
//! real address of the pool buffer that holds them, so every address resolves through the pools of the frame
//! ([`PoolSnapshot::resolve`]). A range must lie in one pool. A range that starts in no pool, or that crosses the end
//! of its pool, is a skipped draw with the address in the message. The checks are generic over the backing of the
//! pools, so they run without a Metal device.

use nxkit_metal::{IndexType, Primitive};

use crate::bindless::Sink;
use crate::detail::{DrawArgs, DrawDetail, StreamDetail};
use crate::draws::{Geom, Shared};
use crate::pools::{Backing, Loc, Miss, PoolSnapshot};
use crate::stats::Skip;
use crate::translate;

#[cfg(test)]
mod tests;

/// Why a range did not resolve, for the line of a skip.
fn why(miss: Miss) -> String {
    match miss {
        Miss::NoPool => "in no pool".into(),
        Miss::CrossesEdge { pool_end } => format!("crosses the end of its pool at {pool_end:#x}"),
        Miss::NoBuffer => "its pool has no buffer".into(),
    }
}

/// Where the vertex stream `s` starts. Only the start must lie in a pool: the size of a stream is an upper bound, and
/// the draw reads what its vertices reach, so a stream longer than its pool is clamped to the pool's `room`.
pub(crate) fn stream_loc<B: Backing>(
    pools: &PoolSnapshot<B>,
    s: &StreamDetail,
) -> Result<Loc, Skip> {
    pools
        .resolve_start_c(s.address, crate::overlay::STREAM)
        .map_err(|m| {
            crate::buf_skip(format_args!(
                "stream {} addr {:#x} size {:#x}: {}",
                s.index,
                s.address,
                s.size,
                why(m)
            ))
        })
}

/// Where `count` indices of `width` bytes at `addr` lie. All of them must lie in one pool.
pub(crate) fn index_loc<B: Backing>(
    pools: &PoolSnapshot<B>,
    addr: u64,
    count: u32,
    width: u64,
) -> Result<Loc, Skip> {
    pools
        .resolve_c(addr, u64::from(count) * width, crate::overlay::STREAM)
        .map_err(|m| {
            crate::buf_skip(format_args!(
                "index addr {addr:#x} count {count}: {}",
                why(m)
            ))
        })
}

/// Where `bytes` at `addr` lie when the GPU reads them as words: a 4 byte aligned guest address and real address in
/// one pool. The error is the reason.
pub(crate) fn word_loc<B: Backing>(
    pools: &PoolSnapshot<B>,
    addr: u64,
    bytes: u64,
) -> Result<Loc, String> {
    if !addr.is_multiple_of(4) {
        return Err("not 4 byte aligned".into());
    }
    let loc = pools
        .resolve_c(addr, bytes, crate::overlay::STREAM)
        .map_err(why)?;
    if !loc.real.is_multiple_of(4) {
        return Err(format!("real address {:#x} not 4 byte aligned", loc.real));
    }
    Ok(loc)
}

/// Checks that `bytes` of indirect arguments at `addr` lie in one pool at a 4 byte aligned address.
pub(crate) fn indirect_args<B: Backing>(
    pools: &PoolSnapshot<B>,
    addr: u64,
    bytes: u64,
) -> Result<Loc, Skip> {
    word_loc(pools, addr, bytes).map_err(|w| {
        crate::buf_skip(format_args!(
            "indirect arguments addr {addr:#x} bytes {bytes}: {w}"
        ))
    })
}

/// Where the index buffer at `addr` starts, and the bytes from there to the end of its pool. An indirect draw takes its
/// index count from the GPU, so the encoder gives Metal all of the pool; Metal reads an index past it as 0.
pub(crate) fn index_room<B: Backing>(
    pools: &PoolSnapshot<B>,
    addr: u64,
) -> Result<(Loc, u64), Skip> {
    let loc = pools
        .resolve_start_c(addr, crate::overlay::STREAM)
        .map_err(|m| crate::buf_skip(format_args!("index addr {addr:#x}: {}", why(m))))?;
    Ok((loc, loc.room.min(u64::from(u32::MAX))))
}

pub(crate) fn geometry(
    sh: &Shared<'_>,
    d: &DrawDetail,
    primitive: Primitive,
    sink: &mut dyn Sink,
) -> Result<(Geom, u32), Skip> {
    Ok(match d.args {
        DrawArgs::Arrays {
            first,
            count,
            instances,
            base_instance,
        } => (
            Geom::Arrays {
                primitive,
                first,
                count,
                instances,
                base_instance,
            },
            base_instance,
        ),
        DrawArgs::Indexed {
            index_type,
            address,
            count,
            instances,
            base_vertex,
            base_instance,
        } => {
            // Metal has no 8-bit indices: the indices are widened to 16 bits into the batch's scratch memory, and the
            // draw reads them there (NVN_INDEX_TYPE_UNSIGNED_BYTE, Ocarina of Time's interface and movie quads).
            let (it, address, loc) = if index_type == 0 {
                let wide = widen_u8_indices(sh, address, count, sink)?;
                (IndexType::U16, wide, None)
            } else {
                let it = translate::index_type(index_type).ok_or(Skip::Index)?;
                let loc = index_loc(sh.pools, address, count, it.bytes())?;
                (it, loc.real, Some(loc))
            };
            (
                Geom::Indexed {
                    primitive,
                    index_type: it,
                    address,
                    loc,
                    count,
                    instances,
                    base_vertex,
                    base_instance,
                },
                base_instance,
            )
        }
        // The GPU reads the first instance with the arguments, so the driver block of an indirect draw has 0.
        DrawArgs::ArraysIndirect { indirect } => {
            let at = indirect_args(sh.pools, indirect, 16)?;
            (
                Geom::Indirect {
                    primitive,
                    indirect: at.real,
                },
                0,
            )
        }
        DrawArgs::IndexedIndirect {
            index_type,
            address,
            indirect,
        } => {
            let it = translate::index_type(index_type).ok_or(Skip::Index)?;
            let args = indirect_args(sh.pools, indirect, 20)?;
            let (index, index_bytes) = index_room(sh.pools, address)?;
            (
                Geom::IndexedIndirect {
                    primitive,
                    index_type: it,
                    address: index.real,
                    index_bytes,
                    indirect: args.real,
                },
                0,
            )
        }
        DrawArgs::MultiIndirectCount { index, max, .. } => {
            let index = match index {
                Some((ty, address)) => {
                    let it = translate::index_type(ty).ok_or(Skip::Index)?;
                    let (loc, room) = index_room(sh.pools, address)?;
                    Some((it, loc.real, room))
                }
                None => None,
            };
            (
                Geom::MultiIndirect {
                    primitive,
                    index,
                    draws: max.min(crate::indirect::MAX_MULTI),
                },
                0,
            )
        }
        DrawArgs::MeshTasks { .. } | DrawArgs::MeshTasksIndirect { .. } => {
            return Err(Skip::Detail);
        }
    })
}

/// Reads `count` 8-bit indices at the guest GPU address `addr`, widens them to 16 bits and puts them into the sink.
/// Returns the address of the 16-bit indices. The CPU reads the guest bytes; the GPU reads the copy in the sink.
fn widen_u8_indices(
    sh: &Shared<'_>,
    addr: u64,
    count: u32,
    sink: &mut dyn Sink,
) -> Result<u64, Skip> {
    sh.guest_offset(addr, u64::from(count))
        .ok_or_else(|| crate::buf_skip(format_args!("8-bit index addr {addr:#x} count {count}")))?;
    let mut bytes = vec![0u8; count as usize];
    if !sh.read_guest(addr, &mut bytes) {
        return Err(crate::buf_skip(format_args!(
            "8-bit index addr {addr:#x} count {count}: unreadable"
        )));
    }
    let wide: Vec<u8> = bytes
        .iter()
        .flat_map(|&b| u16::from(b).to_le_bytes())
        .collect();
    sink.put(&wide)
}
