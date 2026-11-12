//! The cut of a frame into chunks (`weights_of`, `lead_chunk`) and the encode of one chunk (`run_chunk`).

use super::*;

/// The weight of each pass of the plan for the cut into chunks: its commands and one.
pub(super) fn weights_of(plan: &PassPlan) -> Vec<u32> {
    plan.passes
        .iter()
        .map(|p| p.cmds.len() as u32 + 1)
        .collect()
}

/// Whether the encode streams its command buffers (`NXKIT_STREAM_COMMIT`, on unless `0`): the first command buffers of
/// a frame are committed as soon as they are encoded, while the others are encoded.
pub(super) fn stream_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| !matches!(std::env::var("NXKIT_STREAM_COMMIT").as_deref(), Ok("0")))
}

/// The share of the weight of a frame that its lead chunk takes at least: one in this many.
pub(super) const LEAD_SHARE: u64 = 8;

/// The frames with fewer passes than this are not streamed: their encode is short.
pub(super) const STREAM_MIN_PASSES: usize = 8;

/// Puts a lead chunk in front of `ends` (the end of each chunk): the first passes up to about one in [`LEAD_SHARE`] of
/// the weight. Returns true when the lead was added, which needs at least [`STREAM_MIN_PASSES`] passes, a first chunk
/// that is larger than the lead, one chunk more after it (the last command buffer is never in the streamed group), and
/// room for `1 + ends.len() + 1` command buffers within `max_buffers`.
pub(super) fn lead_chunk(weights: &[u32], ends: &mut Vec<usize>, max_buffers: usize) -> bool {
    if weights.len() < STREAM_MIN_PASSES || ends.len() < 2 || 2 + ends.len() > max_buffers {
        return false;
    }
    let total: u64 = weights.iter().map(|w| u64::from(*w)).sum();
    let want = total.div_ceil(LEAD_SHARE).max(1);
    let (mut acc, mut lead) = (0u64, 0usize);
    for (i, w) in weights.iter().enumerate() {
        acc += u64::from(*w);
        lead = i + 1;
        if acc >= want {
            break;
        }
    }
    if lead >= ends[0] {
        return false;
    }
    ends.insert(0, lead);
    true
}

pub(super) fn run_chunk(
    sh: &Shared<'_>,
    cb: &mut CommandBuffer,
    scratch: &Scratch,
    keep: &Arc<Keep>,
    range: Range<usize>,
) -> (EncodeStats, Vec<TexId>) {
    nxkit_metal::autorelease_pool(|| run_chunk_inner(sh, cb, scratch, keep, range))
}

pub(super) fn run_chunk_inner(
    sh: &Shared<'_>,
    cb: &mut CommandBuffer,
    scratch: &Scratch,
    keep: &Arc<Keep>,
    range: Range<usize>,
) -> (EncodeStats, Vec<TexId>) {
    let mut stats = EncodeStats::default();
    let mut copied = Vec::new();
    cb.retain(keep.clone());
    let Ok(tables) = Tables::new(sh.ctx) else {
        stats.skipped[Skip::Encode as usize] += 1;
        return (stats, copied);
    };
    if let Some(p) = sh.prof {
        p.chunk_start(range.start);
    }
    for index in range {
        let Some(pass) = sh.plan.passes.get(index) else {
            continue;
        };
        match pass.kind {
            PassKind::Render => render_pass(sh, cb, &tables, scratch, index, pass, &mut stats),
            PassKind::Compute | PassKind::Copy => {
                compute_pass(sh, cb, &tables, scratch, index, pass, &mut stats)
            }
            PassKind::CpuRead => copied.extend(cpu_read(sh, cb, scratch, index, pass, &mut stats)),
            PassKind::Sync | PassKind::Present => {}
        }
    }
    (stats, copied)
}
