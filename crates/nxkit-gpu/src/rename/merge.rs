//! The kernels of a merge range (`rename`, class `Merge`).
//!
//! A merge range is written by the CPU while a frame runs (the words `C`) and by the GPU (the words `G`), and the two
//! sets of words are disjoint. A frame that binds it reads and writes a copy in its batch slot:
//!
//! - `nxkit_merge_in` runs on the GPU before the first pass of the frame. It copies the whole guest range into the copy
//!   and then takes the words of `C` from the bytes that the CPU thread captured at the flush. The guest words of `G`
//!   hold what the frame before wrote (`nxkit_merge_out` ran at its end), so the state of the GPU carries over.
//! - `nxkit_merge_out` runs on the GPU after the last pass of the frame. It writes the words of `G` from the copy into
//!   the guest range. The CPU never writes those words and the GPU never writes the words of `C` into the guest, so
//!   neither side overwrites the other.
//!
//! Both kernels run as passes of their own with a barrier from all earlier work and one for all later work, so the order
//! of the frames on the queue is the order of the state.

use std::sync::Arc;

use nxkit_metal::{
    ArgumentTable, CommandBuffer, ComputeCommand, Dependency, Scope, Scratch, Stages, Visibility,
};

use super::Capture;
use crate::pipelines::Pipelines;
use crate::stats::Skip;

/// The threads of a threadgroup.
const GROUP: u32 = 64;
/// The bytes of a job.
pub(crate) const JOB_BYTES: usize = 48;

pub(crate) const MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;
struct merge_job {
    device uint *guest;
    device uint *slot;
    device const uint *cvals;
    device const uint *cmask;
    device const uint *gmask;
    uint words;
    uint pad;
};
kernel void nxkit_merge_in(device const merge_job *jp [[buffer(0)]], uint t [[thread_position_in_grid]]) {
    merge_job j = jp[0];
    if (t >= j.words) return;
    bool c = ((j.cmask[t >> 5] >> (t & 31u)) & 1u) != 0u;
    j.slot[t] = c ? j.cvals[t] : j.guest[t];
}
kernel void nxkit_merge_out(device const merge_job *jp [[buffer(0)]], uint t [[thread_position_in_grid]]) {
    merge_job j = jp[0];
    if (t >= j.words) return;
    bool g = ((j.gmask[t >> 5] >> (t & 31u)) & 1u) != 0u;
    if (g) j.guest[t] = j.slot[t];
}
"#;

/// A merge range of a frame: where its copy is and which capture holds its words.
pub(crate) struct MergeRun {
    pub(crate) cap: Arc<Capture>,
    /// The range in `cap`.
    pub(crate) idx: usize,
    /// The real address of the range in guest memory.
    pub(crate) guest: u64,
    /// The real address of the copy.
    pub(crate) slot: u64,
}

/// The bytes of a job as the kernels read them.
pub(crate) fn job_bytes(
    guest: u64,
    slot: u64,
    cvals: u64,
    cmask: u64,
    gmask: u64,
    words: u32,
) -> [u8; JOB_BYTES] {
    let mut b = [0u8; JOB_BYTES];
    for (i, v) in [guest, slot, cvals, cmask, gmask].into_iter().enumerate() {
        b[8 * i..8 * i + 8].copy_from_slice(&v.to_le_bytes());
    }
    b[40..44].copy_from_slice(&words.to_le_bytes());
    b
}

fn mask_bytes(mask: &[u32]) -> Vec<u8> {
    mask.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// Which kernel.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Before the first pass: guest to copy.
    In,
    /// After the last pass: the words of `G` back into guest memory.
    Out,
}

/// Encodes the kernel of `phase` for `runs` as a compute pass of `cb`.
pub(crate) fn encode(
    pipelines: &Pipelines,
    cb: &mut CommandBuffer,
    table: &ArgumentTable,
    scratch: &Scratch,
    runs: &[MergeRun],
    phase: Phase,
) -> Result<(), Skip> {
    if runs.is_empty() {
        return Ok(());
    }
    let entry = match phase {
        Phase::In => "nxkit_merge_in",
        Phase::Out => "nxkit_merge_out",
    };
    let pipeline = pipelines.host_compute("nxkit-merge", MSL, entry)?;
    let mut jobs = Vec::with_capacity(runs.len());
    for r in runs {
        let range = &r.cap.ranges[r.idx];
        let words = (range.start.len() / 4) as u32;
        let push = |b: &[u8]| scratch.push(b).map_err(|_| Skip::Buffer);
        let cvals = if phase == Phase::In {
            push(&range.start)?
        } else {
            0
        };
        let cmask = push(&mask_bytes(&range.cmask))?;
        let gmask = push(&mask_bytes(&range.gmask))?;
        let job = push(&job_bytes(r.guest, r.slot, cvals, cmask, gmask, words))?;
        jobs.push((job, words));
    }
    let mut cp = cb.begin_compute_pass(table).map_err(|_| Skip::Encode)?;
    let enc = |cp: &mut nxkit_metal::ComputePass<'_>, c: ComputeCommand| {
        cp.encode(&c).map_err(|_| Skip::Encode)
    };
    enc(
        &mut cp,
        ComputeCommand::Depend(Dependency {
            scope: Scope::FromEarlierPasses,
            after: Stages::ALL,
            before: Stages::DISPATCH,
            visibility: Visibility::Device,
        }),
    )?;
    enc(&mut cp, ComputeCommand::SetPipeline(pipeline))?;
    for (job, words) in jobs {
        enc(
            &mut cp,
            ComputeCommand::SetBuffer {
                index: 0,
                gpu_addr: job,
            },
        )?;
        enc(
            &mut cp,
            ComputeCommand::DispatchThreads {
                grid: [words, 1, 1],
                threads: [GROUP.min(words), 1, 1],
            },
        )?;
    }
    enc(
        &mut cp,
        ComputeCommand::Depend(Dependency {
            scope: Scope::ToLaterPasses,
            after: Stages::DISPATCH,
            before: Stages::ALL,
            visibility: Visibility::Device,
        }),
    )?;
    cp.end();
    Ok(())
}

#[cfg(test)]
mod tests;
