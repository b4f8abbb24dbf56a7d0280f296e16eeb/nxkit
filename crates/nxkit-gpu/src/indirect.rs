//! The compute pre-pass of the GPU driven draws of a render pass.
//!
//! Two kinds of draw need their arguments changed on the GPU before Metal can draw them:
//!
//! - `MultiDraw{Arrays,Elements}IndirectCount` runs draw `i` only while `i` is below a count that the GPU wrote
//!   earlier in the frame (culling). Metal cannot skip draws by a count in memory. The pre-pass copies the arguments of
//!   every draw up to the maximum and writes zeros past the count, and the render pass issues one indirect draw for
//!   each copy. A draw of zero vertices or instances draws nothing.
//! - `DrawMeshTasksIndirect` reads `{count, first}`, where Metal's indirect mesh draw reads three threadgroup counts.
//!   The pre-pass writes `{count, 1, 1}` and the first workgroup after it. Without a task shader it writes
//!   [`mesh::INDIRECT_SPLITS`](crate::mesh::INDIRECT_SPLITS) grids of at most 1024 workgroups, each with its first
//!   workgroup, and the grids past the count are empty.
//!
//! The choice is a compute pre-pass and not an indirect command buffer: an ICB would need the same kernel to encode
//! the draws, plus GPU encoding of the whole pipeline and argument state, while the copies leave the draw on the normal
//! path of the render pass. The CPU never waits for the GPU. The pre-pass runs once for each render pass that has such
//! draws, before the pass, in the same command buffer: a compute pass cannot run inside a render pass. It waits for
//! all earlier work, and the render pass waits for it. The arguments must be final when the render pass starts, which
//! holds when a dispatch or copy wrote them; a draw of the same pass that writes them through a storage buffer is not
//! seen.

use nxkit_metal::{ArgumentTable, CommandBuffer, ComputeCommand, Scratch};

use crate::detail::DrawArgs;
use crate::draws::{FastMap, Shared};
use crate::geometry::word_loc;
use crate::mesh::{INDIRECT_SPLITS, MAX_GRID};
use crate::pools::{Backing, PoolSnapshot};
use crate::stats::{EncodeStats, Skip};

/// The bytes from one copied record to the next. A record has at most five words and the first workgroup of a mesh
/// grid is at word 4.
pub(crate) const RECORD: u64 = 32;
/// The most draws of one multi-draw that the pre-pass copies. The rest are left out and counted.
pub(crate) const MAX_MULTI: u32 = 4096;
/// The threads of a threadgroup of the kernel.
const GROUP: usize = 64;
/// The bytes of one job of the kernel: three addresses and four words.
const JOB_BYTES: usize = 40;

/// The kernel. A job copies `words` words of draw `i` from `rec` to `out`, or zeros when `i` is not below `*cnt`. A
/// job with no words is a mesh grid: `out` gets the count of `rec[0]` from `base` on, at most `limit`, as `{n, 1, 1}`,
/// and word 4 gets the first workgroup `rec[1] + base`. A job with no output is padding.
const MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;
struct job { device const uint *rec; device const uint *cnt; device uint *out; uint i, words, base, limit; };
kernel void nxkit_indirect(device const job *j [[buffer(0)]], uint t [[thread_position_in_grid]]) {
    job m = j[t];
    if (!m.out) return;
    if (m.words == 0) {
        uint c = m.rec[0];
        m.out[0] = min(c > m.base ? c - m.base : 0u, m.limit);
        m.out[1] = 1u;
        m.out[2] = 1u;
        m.out[4] = m.rec[1] + m.base;
        return;
    }
    bool live = m.i < m.cnt[0];
    for (uint k = 0; k < m.words; k++) m.out[k] = live ? m.rec[k] : 0u;
}
"#;

/// One job of the kernel. `rec` and `cnt` are real addresses.
struct Job {
    rec: u64,
    cnt: u64,
    i: u32,
    words: u32,
    base: u32,
    limit: u32,
}

/// The copies of the GPU driven draws of one render pass, by draw ordinal: the address of the first record, or why the
/// draw has none.
#[derive(Default)]
pub(crate) struct Prepass {
    pub(crate) copies: FastMap<u32, Result<u64, Skip>>,
    /// True when a draw of the pass reads arguments or data that earlier GPU work wrote, so the pass waits for it.
    pub(crate) gpu_driven: bool,
}

/// The jobs of draw `d`, or why it gets none. `None` for a draw that needs no copy. The jobs hold real addresses: the
/// kernel loads through them.
fn jobs_of<B: Backing>(
    pools: &PoolSnapshot<B>,
    args: &DrawArgs,
    task: bool,
    out: &mut Vec<Job>,
) -> Option<Result<(), Skip>> {
    let at = |addr: u64, bytes: u64| word_loc(pools, addr, bytes).ok().map(|l| l.real);
    match *args {
        DrawArgs::MultiIndirectCount {
            index,
            indirect,
            count,
            max,
            stride,
        } => {
            let words = if index.is_some() { 5 } else { 4 };
            let n = max.min(MAX_MULTI);
            let Some(cnt) = at(count, 4).filter(|_| stride.is_multiple_of(4)) else {
                return Some(Err(crate::buf_skip(format_args!(
                    "multi-draw count {count:#x} stride {stride}"
                ))));
            };
            let first = out.len();
            for i in 0..n {
                let rec = indirect.wrapping_add(u64::from(i).wrapping_mul(stride));
                let Some(real) = at(rec, 4 * u64::from(words)) else {
                    out.truncate(first);
                    return Some(Err(crate::buf_skip(format_args!(
                        "multi-draw record {i} at {rec:#x}"
                    ))));
                };
                out.push(Job {
                    rec: real,
                    cnt,
                    i,
                    words,
                    base: 0,
                    limit: 0,
                });
            }
            Some(Ok(()))
        }
        DrawArgs::MeshTasksIndirect { indirect } => {
            let Some(real) = at(indirect, 8) else {
                return Some(Err(crate::buf_skip(format_args!(
                    "mesh draw arguments at {indirect:#x}"
                ))));
            };
            let (splits, limit) = if task {
                (1, u32::MAX)
            } else {
                (INDIRECT_SPLITS, MAX_GRID)
            };
            for k in 0..splits {
                out.push(Job {
                    rec: real,
                    cnt: real,
                    i: 0,
                    words: 0,
                    base: k * MAX_GRID,
                    limit,
                });
            }
            Some(Ok(()))
        }
        _ => None,
    }
}

/// Runs the pre-pass for the draws `range` of a render pass, when one of them needs it.
pub(crate) fn prepass(
    sh: &Shared<'_>,
    cb: &mut CommandBuffer,
    table: &ArgumentTable,
    scratch: &Scratch,
    range: std::ops::Range<u32>,
    stats: &mut EncodeStats,
) -> Prepass {
    let mut out = Prepass::default();
    let mut jobs = Vec::new();
    // The draw ordinal and first job of each draw that gets copies.
    let mut firsts: Vec<(u32, usize)> = Vec::new();
    for n in range {
        let Some(d) = sh.detail.draws.get(n as usize) else {
            continue;
        };
        // A task or mesh shader reads what the culling of the frame wrote, through its storage buffers.
        out.gpu_driven |= d.args.is_indirect() || d.args.is_mesh();
        let at = jobs.len();
        match jobs_of(sh.pools, &d.args, d.program.task.is_some(), &mut jobs) {
            Some(Ok(())) => firsts.push((n, at)),
            Some(Err(why)) => {
                out.copies.insert(n, Err(why));
            }
            None => {}
        }
        if let DrawArgs::MultiIndirectCount { max, .. } = d.args {
            stats.kinds.clamped += max.saturating_sub(MAX_MULTI);
        }
    }
    if firsts.is_empty() {
        return out;
    }
    if jobs.is_empty() {
        // Multi-draws with a maximum of 0: they draw nothing and need no kernel.
        out.copies
            .extend(firsts.into_iter().map(|(n, _)| (n, Ok(0))));
        return out;
    }
    let r = run(sh, cb, table, scratch, &jobs);
    for (n, at) in firsts {
        out.copies
            .insert(n, r.map(|base| base + RECORD * at as u64));
    }
    if r.is_ok() {
        stats.compute_passes += 1;
        stats.kinds.prepasses += 1;
        stats.kinds.prepass_records += jobs.len() as u32;
    }
    out
}

/// Writes the jobs and their outputs into the scratch buffer and encodes the kernel. Gives the address of the output of
/// job 0; job `k` writes at `RECORD * k` from there.
fn run(
    sh: &Shared<'_>,
    cb: &mut CommandBuffer,
    table: &ArgumentTable,
    scratch: &Scratch,
    jobs: &[Job],
) -> Result<u64, Skip> {
    let pipeline = sh
        .pipelines
        .host_compute("nxkit-indirect", MSL, "nxkit_indirect")?;
    let push = |b: &[u8]| {
        scratch
            .push(b)
            .map_err(|e| crate::buf_skip(format_args!("pre-pass {} bytes: {e:?}", b.len())))
    };
    let outputs = push(&vec![0u8; jobs.len() * RECORD as usize])?;
    let padded = jobs.len().next_multiple_of(GROUP);
    let mut bytes = vec![0u8; padded * JOB_BYTES];
    for (k, (j, b)) in jobs
        .iter()
        .zip(bytes.as_chunks_mut::<JOB_BYTES>().0)
        .enumerate()
    {
        let dst = outputs + RECORD * k as u64;
        b[0..8].copy_from_slice(&j.rec.to_le_bytes());
        b[8..16].copy_from_slice(&j.cnt.to_le_bytes());
        b[16..24].copy_from_slice(&dst.to_le_bytes());
        for (w, v) in b[24..40]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip([j.i, j.words, j.base, j.limit])
        {
            w.copy_from_slice(&v.to_le_bytes());
        }
    }
    let list = push(&bytes)?;
    let mut cp = cb.begin_compute_pass(table).map_err(|_| Skip::Encode)?;
    let groups = (padded / GROUP) as u32;
    for c in [
        ComputeCommand::WaitForEarlierPasses,
        ComputeCommand::SetPipeline(pipeline),
        ComputeCommand::SetBuffer {
            index: 0,
            gpu_addr: list,
        },
        ComputeCommand::DispatchThreadgroups {
            groups: [groups, 1, 1],
            threads: [GROUP as u32, 1, 1],
        },
    ] {
        cp.encode(&c).map_err(|_| Skip::Encode)?;
    }
    cp.end();
    Ok(outputs)
}

#[cfg(test)]
mod tests;
