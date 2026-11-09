//! The result of the render pass inference.
//!
//! NVN has no render passes. The analysis in [`crate::analyse`] cuts the frame into passes and picks the load and store
//! action of each attachment. The result is plain data that the Metal backend encodes later.

use std::ops::Range;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::resource::TexId;

/// The two switches of the pass optimiser. The host sets them from its knobs (`nopassmerge`, `noloadstore`) with
/// [`set_options`]. Without a call they read `NXKIT_KNOB` once, and both are on.
static OPTIONS: AtomicU8 = AtomicU8::new(OPT_UNSET);
const OPT_UNSET: u8 = 0x80;
const OPT_NO_MERGE: u8 = 1;
const OPT_NO_LOADSTORE: u8 = 2;

fn options() -> u8 {
    let v = OPTIONS.load(Ordering::Relaxed);
    if v != OPT_UNSET {
        return v;
    }
    let raw = std::env::var("NXKIT_KNOB").unwrap_or_default();
    let has = |name: &str| raw.split(',').any(|k| k.trim() == name);
    let v = (u8::from(has("nopassmerge")) * OPT_NO_MERGE)
        | (u8::from(has("noloadstore")) * OPT_NO_LOADSTORE);
    // A host that called `set_options` meanwhile wins.
    let _ = OPTIONS.compare_exchange(OPT_UNSET, v, Ordering::Relaxed, Ordering::Relaxed);
    OPTIONS.load(Ordering::Relaxed)
}

/// Switches the pass optimiser. `merge` drops render passes that do nothing and merges neighbours after a dropped pass
/// or a change of the target set that left the attachments equal. `loadstore` lets a texture that a game has cleared
/// first in every frame for a long time store nothing at the end of the frame. Both are on by default.
pub fn set_options(merge: bool, loadstore: bool) {
    let v = (u8::from(!merge) * OPT_NO_MERGE) | (u8::from(!loadstore) * OPT_NO_LOADSTORE);
    OPTIONS.store(v, Ordering::Relaxed);
}

/// True when the pass merge (empty passes, merge across a target change) is on.
pub fn merge_on() -> bool {
    options() & OPT_NO_MERGE == 0
}

/// True when the learned store actions are on.
pub fn loadstore_on() -> bool {
    options() & OPT_NO_LOADSTORE == 0
}

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
    /// Another kind of work came before the pass, with no barrier in between.
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

impl AttachmentPlan {
    /// True when the load action is `Load` only because the analysis cannot know that the draws cover the whole
    /// attachment: the pass does not clear, discard, blend, test or sample it, and its result is stored. A host that
    /// knows the draw geometry can turn such a load into `DontCare`.
    pub fn load_is_default(&self) -> bool {
        self.load == LoadAction::Load
            && self.store == StoreAction::Store
            && self.touched
            && !(self.clear_start || self.discard_start || self.old_read || self.sampled)
    }

    /// True when no draw or clear of the pass wrote the attachment: it is loaded and stored for nothing.
    pub fn is_untouched(&self) -> bool {
        !self.touched
    }
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

/// How many passes of one group start for each [`Boundary`] reason.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BoundaryCounts {
    /// Passes that start the frame.
    pub start: u32,
    /// Passes that start because the attachments changed.
    pub targets: u32,
    /// Passes that start after a barrier.
    pub barrier: u32,
    /// Passes that start after another kind of work.
    pub foreign: u32,
    /// Passes that start because of a full clear of a used attachment.
    pub full_clear: u32,
}

impl BoundaryCounts {
    fn slot(&mut self, b: Boundary) -> &mut u32 {
        match b {
            Boundary::Start => &mut self.start,
            Boundary::Targets => &mut self.targets,
            Boundary::Barrier(_) => &mut self.barrier,
            Boundary::Foreign => &mut self.foreign,
            Boundary::FullClear => &mut self.full_clear,
        }
    }

    pub(crate) fn add(&mut self, b: Boundary) {
        *self.slot(b) += 1;
    }

    pub(crate) fn remove(&mut self, b: Boundary) {
        let n = self.slot(b);
        *n = n.saturating_sub(1);
    }

    /// The sum over all reasons.
    pub fn total(&self) -> u32 {
        self.start + self.targets + self.barrier + self.foreign + self.full_clear
    }
}

/// The counters that the host reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PassStats {
    /// Why the render passes start, after the merge.
    pub render_begins: BoundaryCounts,
    /// Why the compute passes start.
    pub compute_begins: BoundaryCounts,
    /// Why the copy, sync, present and CPU read passes start.
    pub other_begins: BoundaryCounts,
    /// Copies without a texture on either side. They are no pass boundary and the encoder does not run them.
    pub buffer_copies_absorbed: u32,
    /// Draws and clears while no render target was set. The GPU does nothing for them, and neither does the encoder.
    pub no_target: u32,
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
    /// Render passes that had no draw and no clear (only a discard). They are not in the plan.
    pub empty_dropped: u32,
    /// Merges of passes that started with a target change (not with a barrier).
    pub merged_after_targets: u32,
    /// Stores that are `DontCare` only because the texture has been cleared first in every frame for a long time.
    pub stores_learned_dead: u32,
    /// The bytes of `stores_learned_dead`.
    pub bytes_learned_dead: u64,
}

/// The passes of one frame. The vectors are reused from frame to frame.
#[derive(Clone, Debug, Default)]
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

impl PassStats {
    /// One line with the boundary counters, for `NXKIT_DIAG=stats`.
    pub fn describe(&self) -> String {
        let group = |name: &str, c: &BoundaryCounts| {
            format!(
                "{name} {} (start {} targets {} barrier {} foreign {} full_clear {})",
                c.total(),
                c.start,
                c.targets,
                c.barrier,
                c.foreign,
                c.full_clear
            )
        };
        format!(
            "{}, {}, {}, render before merge {}, buffer copies absorbed {}, no target {}, empty dropped {}, \
             merged after targets {}, learned dead stores {} ({} bytes)",
            group("render", &self.render_begins),
            group("compute", &self.compute_begins),
            group("other", &self.other_begins),
            self.render_passes_before_merge,
            self.buffer_copies_absorbed,
            self.no_target,
            self.empty_dropped,
            self.merged_after_targets,
            self.stores_learned_dead,
            self.bytes_learned_dead
        )
    }
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
