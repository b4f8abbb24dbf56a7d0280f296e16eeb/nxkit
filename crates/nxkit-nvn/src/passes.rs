//! The result of the render pass inference.
//!
//! NVN has no render passes. The analysis in [`crate::analyse`] cuts the frame into passes and picks the load and store
//! action of each attachment. The result is plain data that the Metal backend encodes later.

use std::ops::Range;

use crate::resource::TexId;

/// What a pass does with the old contents of an attachment at its start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadAction {
    /// Read the old contents.
    Load,
    /// Clear the attachment.
    Clear,
    /// The old contents are not needed.
    DontCare,
}

/// What a pass does with the new contents of an attachment at its end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreAction {
    /// Write the contents to memory.
    Store,
    /// The contents are dead.
    DontCare,
}

/// The place of an attachment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    /// A colour slot.
    Color(u8),
    /// Depth and stencil.
    Depth,
}

/// What kind of work a pass holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PassKind {
    /// Draws and clears into attachments.
    Render,
    /// Compute dispatches.
    Compute,
    /// Copies, blits and downsamples.
    Copy,
    /// A queue fence or wait.
    Sync,
    /// A present.
    Present,
    /// A CPU readback.
    CpuRead,
}

/// Why a pass starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Boundary {
    /// The first pass of the frame.
    Start,
    /// The attachments changed.
    Targets,
    /// A barrier with these bits came before the pass.
    Barrier(u32),
    /// Another kind of work came before the pass.
    Foreign,
    /// A full clear of an attachment that the pass before had already used.
    FullClear,
}

/// One attachment of a render pass.
#[derive(Clone, Copy, Debug)]
pub struct AttachmentPlan {
    /// The texture.
    pub tex: TexId,
    /// Where it is attached.
    pub slot: Slot,
    /// The mip, layer and format selected for this attachment.
    pub view: crate::frame::TargetView,
    /// The size of the first mip level in bytes.
    pub bytes: u64,
    /// The chosen load action.
    pub load: LoadAction,
    /// The chosen store action.
    pub store: StoreAction,
    /// True when the attachment can live in tile memory only.
    pub memoryless: bool,
    /// The store is kept only because the host assumes that a later frame may read it.
    pub store_assumed: bool,
    pub(crate) clear_start: bool,
    pub(crate) discard_start: bool,
    pub(crate) old_read: bool,
    pub(crate) sampled: bool,
    pub(crate) discard_end: bool,
    pub(crate) touched: bool,
}

/// One pass.
#[derive(Clone, Debug)]
pub struct Pass {
    /// What the pass does.
    pub kind: PassKind,
    /// Why it starts.
    pub begin: Boundary,
    /// The commands of the pass, as indices into the frame.
    pub cmds: Range<u32>,
    /// The attachments, as indices into [`PassPlan::attachments`].
    pub attachments: Range<usize>,
    pub(crate) reads: Range<usize>,
    pub(crate) writes: Range<usize>,
}

/// The counters that the host reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PassStats {
    /// Render passes before the merge.
    pub render_passes_before_merge: u32,
    /// Render passes after the merge.
    pub render_passes: u32,
    /// All passes, including copy and compute.
    pub passes: u32,
    /// Bytes that the passes load.
    pub bytes_loaded: u64,
    /// Bytes that the passes store.
    pub bytes_stored: u64,
    /// Part of `bytes_stored` that is stored only because a later frame may read it.
    pub bytes_stored_assumed: u64,
    /// Bytes of attachments that stay in tile memory.
    pub bytes_memoryless: u64,
    /// The bytes that a load and a store of every attachment of every unmerged pass would move.
    pub bytes_naive: u64,
    /// `bytes_naive` minus loaded and stored bytes.
    pub bytes_saved: u64,
}

/// The passes of one frame. The vectors are reused from frame to frame.
#[derive(Debug, Default)]
pub struct PassPlan {
    /// The passes in order.
    pub passes: Vec<Pass>,
    /// The attachments of all render passes.
    pub attachments: Vec<AttachmentPlan>,
    pub(crate) reads: Vec<TexId>,
    pub(crate) writes: Vec<TexId>,
    /// The counters.
    pub stats: PassStats,
}

impl PassPlan {
    /// An empty plan.
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn clear(&mut self) {
        self.passes.clear();
        self.attachments.clear();
        self.reads.clear();
        self.writes.clear();
        self.stats = PassStats::default();
    }

    /// The attachments of a pass.
    pub fn attachments_of(&self, p: &Pass) -> &[AttachmentPlan] {
        &self.attachments[p.attachments.clone()]
    }

    /// The textures that a pass samples.
    pub fn reads_of(&self, p: &Pass) -> &[TexId] {
        &self.reads[p.reads.clone()]
    }
}
