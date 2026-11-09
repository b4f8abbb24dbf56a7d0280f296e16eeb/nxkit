//! The copies between block shadows and their compressed textures around a render pass (`textures::blocks`).
//!
//! Metal copies a compressed texture to and from a buffer, not to an uncompressed texture, so each copy goes through
//! one buffer per pass: the blocks of every shadow at a 256 byte aligned offset.

use std::sync::Arc;

use nxkit_metal::{
    ArgumentTable, ComputeCommand, ComputePass, DataBuffer, Texture, TextureRegion, Use,
};

use super::*;
use crate::textures::BlockTarget;

/// The offset of each shadow's blocks in the buffer of a pass, and the bytes of the buffer.
pub(super) fn offsets(sizes: &[u64]) -> (Vec<u64>, u64) {
    let mut at = 0;
    let offsets = sizes
        .iter()
        .map(|s| {
            let o = at;
            at += s.next_multiple_of(256);
            o
        })
        .collect();
    (offsets, at)
}

/// Which way the blocks go.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Way {
    /// From the compressed texture into the shadows, before a pass that loads them.
    Load,
    /// From the shadows into the compressed texture, after a pass that stores them.
    Store,
}

/// Encodes the copies of `blocks` in one compute pass. A load waits for all earlier work, which wrote the compressed
/// texture; a store waits for the attachment writes of the render pass before. The pass that loads must itself wait
/// for the blit stage of this pass. Returns false when a copy could not be encoded.
pub(super) fn copy(
    ctx: &Context,
    cb: &mut CommandBuffer,
    table: &ArgumentTable,
    blocks: &[&BlockTarget],
    way: Way,
) -> bool {
    if blocks.is_empty() {
        return true;
    }
    let sizes: Vec<u64> = blocks.iter().map(|b| b.bytes()).collect();
    let (at, total) = offsets(&sizes);
    let Ok(buffer) = DataBuffer::new_unfilled(ctx, total).map(Arc::new) else {
        return false;
    };
    let Ok(mut cp) = cb.begin_compute_pass(table) else {
        return false;
    };
    let first = match way {
        Way::Load => ComputeCommand::WaitForEarlierPasses,
        Way::Store => {
            ComputeCommand::Depend(Dependency::between(Use::AttachmentWrite, Use::CopyRead))
        }
    };
    let ok = cp.encode(&first).is_ok() && encode(&mut cp, blocks, &buffer, &at, way);
    cp.end();
    ok
}

/// The source and the destination of the copy of `b`: a texture and its box each.
fn sides(b: &BlockTarget, way: Way) -> ((Texture, TextureRegion), (Texture, TextureRegion)) {
    let d = b.shadow.desc();
    let shadow = (
        b.shadow.clone(),
        TextureRegion::whole(0, 0, [d.width, d.height, 1]),
    );
    let target = (b.target.clone(), b.region);
    match way {
        Way::Load => (target, shadow),
        Way::Store => (shadow, target),
    }
}

fn encode(
    cp: &mut ComputePass<'_>,
    blocks: &[&BlockTarget],
    buffer: &Arc<DataBuffer>,
    at: &[u64],
    way: Way,
) -> bool {
    // The source of each copy goes into the buffer, then the buffer into the destination.
    for (b, o) in blocks.iter().zip(at) {
        let ((tex, region), _) = sides(b, way);
        if cp
            .copy_buffer_box(true, &tex, &region, buffer, (*o, b.row_bytes))
            .is_err()
        {
            return false;
        }
    }
    if cp
        .encode(&ComputeCommand::Depend(Dependency::within_pass(
            Use::CopyWrite,
            Use::CopyRead,
        )))
        .is_err()
    {
        return false;
    }
    blocks.iter().zip(at).all(|(b, o)| {
        let (_, (tex, region)) = sides(b, way);
        cp.copy_buffer_box(false, &tex, &region, buffer, (*o, b.row_bytes))
            .is_ok()
    })
}
