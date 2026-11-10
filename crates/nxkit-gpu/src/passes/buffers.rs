//! `nvnCommandBufferCopyBufferToBuffer` and `nvnCommandBufferClearBuffer` on the GPU.
//!
//! The pools are Metal buffers over guest memory, so a kernel that writes through a real address writes guest
//! memory. A game fills the records of its indirect draws with a buffer copy from the output of its culling and
//! resets counters with a buffer clear; without them the GPU draws what the CPU saw in memory.
//!
//! One host kernel does both. A job is a source and destination address (real, resolved through the pool snapshot),
//! a byte count and a mode:
//!
//! - Words: when the addresses and the size are multiples of 4, a thread moves 4 words. Otherwise a thread moves 16
//!   bytes. A fill writes the 32-bit pattern from the first byte of the range on, in little-endian order, so a range
//!   of any size and alignment gets the same bytes the hardware writes for a size that is a multiple of 4.
//! - Overlap: the ranges of a copy may overlap (NVN does not forbid it). Threads running in parallel would race, so
//!   an overlapping copy runs on one thread in the direction that `memmove` takes. That is slow for a large range and
//!   is counted (`copy.buffer-serial`). A copy of a range onto itself does nothing.
//!
//! The order is the order of the commands: the commands are a pass of their own (see `nxkit_nvn::analyse`), and the
//! pass starts with a barrier from all earlier work, has a barrier around each job, and the barrier plan makes the
//! passes after it wait (see `barriers`).

use nxkit_metal::{ComputeCommand, ComputePass, Scratch};
use nxkit_nvn::diag::Reason;

use crate::detail::{CopyDetail, CopyKind};
use crate::draws::Shared;
use crate::pools::{Backing, PoolSnapshot};
use crate::stats::Skip;

/// The threads of a threadgroup.
const GROUP: u32 = 64;
/// The most threads of one dispatch. A larger range gives each thread more units.
const MAX_THREADS: u64 = 1 << 26;
/// The bytes of a job.
const JOB_BYTES: usize = 48;

const FILL: u32 = 1;
const WORDS: u32 = 2;
const BACKWARD: u32 = 4;

/// The kernel. Thread `t` handles the units `[t * per, (t + 1) * per)`: words in word mode, bytes otherwise.
const MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;
struct job { device uchar *src; device uchar *dst; ulong bytes; uint value; uint mode; ulong per; };
kernel void nxkit_buffers(device const job *jp [[buffer(0)]], uint t [[thread_position_in_grid]]) {
    job j = jp[0];
    bool fill = (j.mode & 1u) != 0u, word = (j.mode & 2u) != 0u, back = (j.mode & 4u) != 0u;
    ulong units = word ? (j.bytes >> 2) : j.bytes;
    ulong first = ulong(t) * j.per;
    if (first >= units) return;
    ulong end = min(first + j.per, units);
    if (word) {
        device uint *d = (device uint *)j.dst;
        device const uint *s = (device const uint *)j.src;
        if (back) {
            for (ulong i = end; i > first; i--) d[i - 1] = fill ? j.value : s[i - 1];
        } else {
            for (ulong i = first; i < end; i++) d[i] = fill ? j.value : s[i];
        }
    } else {
        if (back) {
            for (ulong i = end; i > first; i--) j.dst[i - 1] = fill ? uchar(j.value >> (8u * uint(i - 1) & 24u)) : j.src[i - 1];
        } else {
            for (ulong i = first; i < end; i++) j.dst[i] = fill ? uchar(j.value >> (8u * uint(i) & 24u)) : j.src[i];
        }
    }
}
"#;

/// One run of the kernel, as it is written to the job buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Job {
    src: u64,
    dst: u64,
    bytes: u64,
    value: u32,
    mode: u32,
    per: u64,
    /// The threads of the dispatch.
    pub(crate) threads: u32,
    /// True when the ranges overlap and one thread runs the job in order.
    pub(crate) serial: bool,
}

/// The units each thread handles and the threads, for `units` units with `base` units to a thread at least.
fn spread(units: u64, base: u64) -> (u64, u32) {
    let per = base.max(units.div_ceil(MAX_THREADS));
    (per, units.div_ceil(per) as u32)
}

impl Job {
    /// The job that copies `bytes` from the real address `src` to `dst`. `None` when there is nothing to do: no bytes,
    /// or the same range.
    pub(crate) fn copy(src: u64, dst: u64, bytes: u64) -> Option<Self> {
        if bytes == 0 || src == dst {
            return None;
        }
        let words = (src | dst | bytes) & 3 == 0;
        let units = if words { bytes / 4 } else { bytes };
        let overlap = src < dst.saturating_add(bytes) && dst < src.saturating_add(bytes);
        let (per, threads) = if overlap {
            (units, 1)
        } else {
            spread(units, if words { 4 } else { 16 })
        };
        Some(Self {
            src,
            dst,
            bytes,
            value: 0,
            mode: if words { WORDS } else { 0 } | if overlap && dst > src { BACKWARD } else { 0 },
            per,
            threads,
            serial: overlap,
        })
    }

    /// The job that writes `value` over `bytes` from the real address `dst` on.
    pub(crate) fn fill(dst: u64, bytes: u64, value: u32) -> Option<Self> {
        if bytes == 0 {
            return None;
        }
        let words = (dst | bytes) & 3 == 0;
        let units = if words { bytes / 4 } else { bytes };
        let (per, threads) = spread(units, if words { 4 } else { 16 });
        Some(Self {
            src: 0,
            dst,
            bytes,
            value,
            mode: FILL | if words { WORDS } else { 0 },
            per,
            threads,
            serial: false,
        })
    }

    /// The bytes of the job as the kernel reads them.
    pub(crate) fn to_bytes(self) -> [u8; JOB_BYTES] {
        let mut b = [0u8; JOB_BYTES];
        b[0..8].copy_from_slice(&self.src.to_le_bytes());
        b[8..16].copy_from_slice(&self.dst.to_le_bytes());
        b[16..24].copy_from_slice(&self.bytes.to_le_bytes());
        b[24..28].copy_from_slice(&self.value.to_le_bytes());
        b[28..32].copy_from_slice(&self.mode.to_le_bytes());
        b[32..40].copy_from_slice(&self.per.to_le_bytes());
        b
    }
}

/// The real address of `bytes` at the guest address `addr`, or why they do not lie whole in one pool.
fn real<B: Backing>(pools: &PoolSnapshot<B>, addr: u64, bytes: u64) -> Result<u64, String> {
    pools
        .resolve(addr, bytes)
        .map(|l| l.real)
        .map_err(|m| format!("{addr:#x} + {bytes:#x}: {m:?}"))
}

/// The job of a buffer copy or fill entry, with the addresses resolved through `pools`. `Ok(None)` when it does
/// nothing. The error names the range that did not resolve.
pub(crate) fn job_of<B: Backing>(
    pools: &PoolSnapshot<B>,
    c: &CopyDetail,
) -> Result<Option<Job>, String> {
    match c.kind {
        CopyKind::BufferToBuffer => {
            if c.bytes == 0 {
                return Ok(None);
            }
            let src = real(pools, c.src_addr, c.bytes).map_err(|w| format!("source {w}"))?;
            let dst = real(pools, c.dst_addr, c.bytes).map_err(|w| format!("destination {w}"))?;
            Ok(Job::copy(src, dst, c.bytes))
        }
        CopyKind::BufferFill => {
            if c.bytes == 0 {
                return Ok(None);
            }
            let dst = real(pools, c.dst_addr, c.bytes).map_err(|w| format!("destination {w}"))?;
            Ok(Job::fill(dst, c.bytes, c.value))
        }
        _ => Err(format!("{:?} is no buffer command", c.kind)),
    }
}

/// Prints a buffer command under the `copy` category (at most 400 lines).
fn print(c: &CopyDetail, what: &str) {
    static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    if crate::diag("copy") && crate::diag::first(&SHOWN, 400) {
        eprintln!(
            "DIAG copy buffer {:?} src {:#x} dst {:#x} bytes {:#x} value {:#x}: {what}",
            c.kind, c.src_addr, c.dst_addr, c.bytes, c.value
        );
    }
}

/// Encodes a buffer copy or fill into the compute pass `cp`, between two barriers.
pub(super) fn run(
    sh: &Shared<'_>,
    cp: &mut ComputePass<'_>,
    scratch: &Scratch,
    c: &CopyDetail,
) -> Result<(), Skip> {
    let job = match job_of(sh.pools, c) {
        Ok(Some(job)) => job,
        Ok(None) => {
            print(c, "nothing to do");
            return Ok(());
        }
        Err(why) => {
            print(c, &format!("skipped: {why}"));
            return Err(super::copy::copy_skip(format_args!(
                "buffer command {:?} src {:#x} dst {:#x} bytes {:#x}: {why}",
                c.kind, c.src_addr, c.dst_addr, c.bytes
            )));
        }
    };
    let pipeline = sh
        .pipelines
        .host_compute("nxkit-buffers", MSL, "nxkit_buffers")?;
    let list = scratch
        .push(&job.to_bytes())
        .map_err(|e| super::copy::copy_skip(format_args!("buffer command job: scratch {e:?}")))?;
    for cmd in [
        ComputeCommand::Barrier,
        ComputeCommand::SetPipeline(pipeline),
        ComputeCommand::SetBuffer {
            index: 0,
            gpu_addr: list,
        },
        ComputeCommand::DispatchThreads {
            grid: [job.threads, 1, 1],
            threads: [GROUP.min(job.threads), 1, 1],
        },
        ComputeCommand::Barrier,
    ] {
        cp.encode(&cmd).map_err(|_| Skip::Encode)?;
    }
    nxkit_nvn::diag::note_n(Reason::BufferOps, 1);
    if job.serial {
        nxkit_nvn::diag::note_n(Reason::BufferOpsSerial, 1);
    }
    print(c, "encoded");
    Ok(())
}

#[cfg(test)]
mod tests;
