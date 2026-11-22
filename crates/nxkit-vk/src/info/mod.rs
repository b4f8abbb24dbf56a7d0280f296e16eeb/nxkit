//! Decoders of the structs that the game passes: each reads a `Vk*CreateInfo` (or an array of regions) from guest
//! memory into a plain Rust value. The values still name other objects by handle. The front end resolves them.
//!
//! The ABI layer calls a decoder with [`crate::mem::GuestMemory`] and the address that the game gave, then the front
//! end with the value. Tests call them with [`crate::mem::TestMemory`].

mod command;
mod descriptor;
mod pipeline;
mod resource;

pub use command::*;
pub use descriptor::*;
pub use pipeline::*;
pub use resource::*;

pub use crate::desc::SubresourceRange;

use crate::desc::{Rect2D, Viewport};
use crate::layout::{extent3d, offset3d, rect2d, viewport};
use crate::mem::Mem;
use crate::result::VkResult;

/// Reads `n` elements of `stride` bytes at `addr` with `one`. A null array or a zero count gives no elements.
pub fn array<M: Mem, T>(
    m: &M,
    addr: u64,
    n: u32,
    stride: u64,
    mut one: impl FnMut(&M, u64) -> VkResult<T>,
) -> VkResult<Vec<T>> {
    if addr == 0 || n == 0 {
        return Ok(Vec::new());
    }
    let n = crate::mem::check_count(n)?;
    (0..u64::from(n))
        .map(|i| one(m, addr + i * stride))
        .collect()
}

/// Reads a `VkExtent3D`.
pub fn extent3(m: &impl Mem, at: u64) -> VkResult<[u32; 3]> {
    Ok([
        m.u32(at + extent3d::WIDTH)?,
        m.u32(at + extent3d::HEIGHT)?,
        m.u32(at + extent3d::DEPTH)?,
    ])
}

/// Reads a `VkOffset3D`.
pub fn offset3(m: &impl Mem, at: u64) -> VkResult<[i32; 3]> {
    Ok([
        m.i32(at + offset3d::X)?,
        m.i32(at + offset3d::Y)?,
        m.i32(at + offset3d::Z)?,
    ])
}

/// Reads a `VkRect2D`.
pub fn rect(m: &impl Mem, at: u64) -> VkResult<Rect2D> {
    Ok(Rect2D {
        x: m.i32(at + rect2d::OFFSET)?,
        y: m.i32(at + rect2d::OFFSET + 4)?,
        width: m.u32(at + rect2d::EXTENT)?,
        height: m.u32(at + rect2d::EXTENT + 4)?,
    })
}

/// Reads `n` `VkRect2D`.
pub fn rects(m: &impl Mem, at: u64, n: u32) -> VkResult<Vec<Rect2D>> {
    array(m, at, n, rect2d::SIZE, |m, a| rect(m, a))
}

/// Reads a `VkViewport`.
pub fn viewport_at(m: &impl Mem, at: u64) -> VkResult<Viewport> {
    Ok(Viewport {
        x: m.f32(at + viewport::X)?,
        y: m.f32(at + viewport::Y)?,
        width: m.f32(at + viewport::WIDTH)?,
        height: m.f32(at + viewport::HEIGHT)?,
        min_depth: m.f32(at + viewport::MIN_DEPTH)?,
        max_depth: m.f32(at + viewport::MAX_DEPTH)?,
    })
}

/// Reads `n` `VkViewport`.
pub fn viewports(m: &impl Mem, at: u64, n: u32) -> VkResult<Vec<Viewport>> {
    array(m, at, n, viewport::SIZE, |m, a| viewport_at(m, a))
}

/// Reads `n` handles.
pub fn handles(m: &impl Mem, at: u64, n: u32) -> VkResult<Vec<u64>> {
    if at == 0 || n == 0 {
        return Ok(Vec::new());
    }
    m.u64s(at, n)
}
