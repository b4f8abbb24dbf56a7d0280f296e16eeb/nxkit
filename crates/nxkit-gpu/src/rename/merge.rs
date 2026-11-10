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
pub(crate) const JOB_BYTES: usize = 64;

pub(crate) const MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;
struct merge_job {
    device uint *guest;
    device uint *slot;
    device const uint *cvals;
    device const uint *cmask;
    device const uint *gmask;
    device uint *copy2;
    device const uint *cprefix;
    uint words;
    uint pad;
};
kernel void nxkit_merge_in(device const merge_job *jp [[buffer(0)]], uint t [[thread_position_in_grid]]) {
    merge_job j = jp[0];
    if (t >= j.words) return;
    uint block = j.cmask[t >> 5];
    bool c = ((block >> (t & 31u)) & 1u) != 0u;
    // The words of the CPU are packed in `cvals`; `cprefix` counts those before each block of 32 words.
    uint v = c ? j.cvals[j.cprefix[t >> 5] + popcount(block & ((1u << (t & 31u)) - 1u))] : j.guest[t];
    j.slot[t] = v;
    // A probe keeps a second copy of the state at the start of the frame.
    if (j.copy2 != nullptr) j.copy2[t] = v;
}
kernel void nxkit_probe_out(device const merge_job *jp [[buffer(0)]], uint t [[thread_position_in_grid]]) {
    merge_job j = jp[0];
    if (t >= j.words) return;
    uint v = j.slot[t];
    if (v != j.copy2[t]) j.guest[t] = v;
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
    /// For a probe, the real address of the copy as it started; 0 for a merge range.
    pub(crate) copy2: u64,
}

/// The bytes of a job as the kernels read them: the addresses of the guest range, the copy, the packed words of the CPU,
/// the mask of the CPU, the mask for the end of the frame, the copy of the start state and the block counts, in this
/// order, and the number of words.
pub(crate) fn job_bytes(addrs: [u64; 7], words: u32) -> [u8; JOB_BYTES] {
    let mut b = [0u8; JOB_BYTES];
    for (i, v) in addrs.into_iter().enumerate() {
        b[8 * i..8 * i + 8].copy_from_slice(&v.to_le_bytes());
    }
    b[56..60].copy_from_slice(&words.to_le_bytes());
    b
}

/// The bytes of a mask. An empty mask is one word of zeros, so that it still has an address.
fn mask_bytes(mask: &[u32]) -> Vec<u8> {
    if mask.is_empty() {
        return vec![0; 4];
    }
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

/// Encodes the kernel of `phase` for `runs` as a compute pass of `cb`. At the end of a frame a merge range runs
/// `nxkit_merge_out` (the words of `G`) and a probe `nxkit_probe_out` (every word that differs from the start copy).
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
    // The kernel of each run: its entry and its job.
    let mut jobs: Vec<(&'static str, u64, u32)> = Vec::with_capacity(runs.len());
    for r in runs {
        let range = &r.cap.ranges[r.idx];
        let words = range.len / 4;
        let push = |b: &[u8]| scratch.push(b).map_err(|_| Skip::Buffer);
        let entry = match (phase, r.copy2 != 0) {
            (Phase::In, _) => "nxkit_merge_in",
            (Phase::Out, false) => "nxkit_merge_out",
            (Phase::Out, true) => "nxkit_probe_out",
        };
        let (cvals, cprefix) = if phase == Phase::In {
            // The words of the CPU, packed (an empty list still needs an address).
            let vals: Vec<u8> = range.cvals.iter().flat_map(|w| w.to_le_bytes()).collect();
            (
                push(if vals.is_empty() { &[0; 4] } else { &vals })?,
                push(&mask_bytes(&range.cprefix))?,
            )
        } else {
            (0, 0)
        };
        let cmask = push(&mask_bytes(&range.cmask))?;
        let gmask = push(&mask_bytes(&range.gmask))?;
        let job = push(&job_bytes(
            [r.guest, r.slot, cvals, cmask, gmask, r.copy2, cprefix],
            words,
        ))?;
        jobs.push((entry, job, words));
    }
    jobs.sort_by_key(|j| j.0);
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
    let mut current = "";
    for (entry, job, words) in jobs {
        if entry != current {
            let pipeline = pipelines.host_compute("nxkit-merge", MSL, entry)?;
            enc(&mut cp, ComputeCommand::SetPipeline(pipeline))?;
            current = entry;
        }
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
