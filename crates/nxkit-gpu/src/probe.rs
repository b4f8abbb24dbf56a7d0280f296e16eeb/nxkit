//! The observer interface of the opt-in analyses (W29).
//!
//! The encode path reports what it does through these hooks and knows no analysis module. `Gpu` installs the
//! analyses (`gpu::PROBES`): `nanscan` watches render and compute passes, `lum` captures stored colour targets,
//! `passprof` stamps the end of each pass ([`PassClock`]) and `meshvis` counts mesh draws in the visibility buffer of
//! the GPU counters ([`MeshCounts`]). Each analysis keeps its own `NXKIT_DIAG` switch, so an installed hook that is
//! off returns at once, as the direct calls did.

use std::ops::Range;

use nxkit_metal::{
    ArgumentTable, CommandBuffer, ComputePass, Context, RenderPass, RenderPassDesc, Texture,
};
use nxkit_nvn::passes::Pass;
use nxkit_nvn::resource::TexId;

use crate::drawctx::{Prep, Shared};

/// An analysis that watches one pass: it encodes its part right after the pass.
pub(crate) trait PassWatch {
    /// Encodes what runs after the pass.
    fn after(self: Box<Self>, sh: &Shared<'_>, cb: &mut CommandBuffer);
}

/// Before render pass `index` with the draws `draws` and the Metal pass `desc`: what to run after it, if anything.
pub(crate) type RenderHook = fn(
    sh: &Shared<'_>,
    cb: &mut CommandBuffer,
    index: usize,
    pass: &Pass,
    draws: Range<u32>,
    desc: &RenderPassDesc,
) -> Option<Box<dyn PassWatch>>;

/// Before compute pass `index`: what to run after it, if anything.
pub(crate) type ComputeHook = fn(
    sh: &Shared<'_>,
    cb: &mut CommandBuffer,
    index: usize,
    pass: &Pass,
) -> Option<Box<dyn PassWatch>>;

/// A colour target that a render pass stored: the pass's first command `cmd`, the texture `id` and its Metal
/// texture with the depth plane.
pub(crate) type StoredHook = fn(
    ctx: &Context,
    cb: &mut CommandBuffer,
    table: &ArgumentTable,
    cmd: u32,
    id: TexId,
    tex: &Texture,
    depth_plane: u32,
);

/// The hooks of the encode path.
#[derive(Clone, Copy)]
pub(crate) struct Probes {
    /// Before each render pass.
    pub(crate) render: RenderHook,
    /// Before each compute pass.
    pub(crate) compute: ComputeHook,
    /// Whether this frame wants the stored colour targets of the focused pass.
    pub(crate) stored_on: fn() -> bool,
    /// One stored colour target.
    pub(crate) stored: StoredHook,
}

/// The pass profile of a frame: a GPU timestamp at the start of each chunk and at the end of each pass.
pub(crate) trait PassClock: Sync {
    /// Chunk of passes starting at pass `index` begins.
    fn chunk_start(&self, index: usize);
    /// The end of render pass `index`.
    fn end_pass_render(&self, rp: &mut RenderPass<'_>, index: usize);
    /// The end of compute pass `index`.
    fn end_pass_compute(&self, cp: &mut ComputePass<'_>, index: usize);
}

/// Where a GPU address lies in the raw global memory: the range of the current table and the pool that shaders wrote.
pub(crate) struct Where {
    pub(crate) table: Option<(u64, u64)>,
    pub(crate) written: Option<(u64, u64)>,
}

/// Mesh draws that an analysis counts with offsets of its own in the visibility buffer of the GPU counters.
pub(crate) trait MeshCounts: Send + Sync {
    /// The offsets it wants in the visibility buffer.
    fn wanted(&self) -> usize;
    /// Fixes its first offset index and the room of the buffer after the game's offsets.
    fn bind(&mut self, base: usize, room: usize);
    /// True when a draw of render pass `index` has an offset.
    fn has_pass(&self, index: usize) -> bool;
    /// The byte offset that mesh draw `ord` counts into.
    fn offset(&self, ord: u32) -> Option<u64>;
    /// The state that mesh draw `ord` really used.
    fn note(&self, ord: u32, p: &Prep);
    /// A Metal draw of mesh draw `ord` was encoded with its offset set.
    fn encoded(&self, ord: u32);
    /// The lines of the frame; `shared` says whether a report of the game counts a draw, `samples` reads an offset
    /// index of the buffer and `record` a workgroup count in guest memory.
    fn report(
        &self,
        shared: &dyn Fn(u32) -> bool,
        samples: &dyn Fn(u64) -> Option<u64>,
        record: &dyn Fn(u64) -> Option<u32>,
    ) -> Vec<String>;
    /// The lines of the indirect draws; `words` reads 4 words in guest memory, `place` places a GPU address.
    fn record_lines(
        &self,
        words: &dyn Fn(u64) -> Option<[u32; 4]>,
        place: &dyn Fn(u64) -> Where,
    ) -> Vec<String>;
}
