//! The indirect compute dispatch (`nvnCommandBufferDispatchComputeIndirect`).
//!
//! The group counts live in guest memory that an earlier dispatch of the frame may write, so the CPU does not know
//! them. Metal reads them itself (`DispatchThreadgroupsIndirect`). The shader may also read the counts as the number of
//! workgroups, from the dispatch words of its driver block (`DRV_DISPATCH`). A one-thread kernel copies the three
//! counts there right before the dispatch.

use nxkit_metal::{ComputeCommand, ComputePass, Dependency, Scope, Scratch, Stages, Visibility};

use crate::bindless::{DRV_DISPATCH, PUSH_BANK};
use crate::draws::Shared;
use crate::programs::{BindKind, StageProgram};
use crate::stats::Skip;

/// The driver block pointers a job can patch.
const SLOTS: usize = 8;
/// The bytes of a job: args address, record address, slot count, dispatch word offset, then the slots.
const JOB_BYTES: usize = 8 + 8 + 4 + 4 + 4 * SLOTS;

const MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;
struct job { device const ulong *args; device const uint *rec; uint n; uint at; uint slots[8]; };
kernel void nxkit_dispatch_size(constant job &j [[buffer(0)]]) {
    for (uint k = 0; k < j.n && k < 8; k++) {
        device uint *d = (device uint *)j.args[j.slots[k]];
        if (!d) continue;
        for (uint i = 0; i < 3; i++) d[j.at / 4 + i] = j.rec[i];
    }
}
"#;

/// The member indices of the argument buffer that hold the driver block pointer of a program, which is the only
/// place the dispatch size is read from. Push constants (bank 24) are not the driver block.
pub(super) fn drv_slots(p: &StageProgram) -> Vec<u32> {
    p.binds
        .iter()
        .filter(|b| b.kind == BindKind::DrvCb && b.bank != PUSH_BANK)
        .map(|b| b.arg_id)
        .take(SLOTS)
        .collect()
}

/// The bytes of the job for the kernel.
pub(super) fn job_bytes(args: u64, rec: u64, slots: &[u32]) -> [u8; JOB_BYTES] {
    let mut b = [0u8; JOB_BYTES];
    b[0..8].copy_from_slice(&args.to_le_bytes());
    b[8..16].copy_from_slice(&rec.to_le_bytes());
    b[16..20].copy_from_slice(&(slots.len().min(SLOTS) as u32).to_le_bytes());
    b[20..24].copy_from_slice(&(DRV_DISPATCH as u32).to_le_bytes());
    for (i, s) in slots.iter().take(SLOTS).enumerate() {
        b[24 + 4 * i..28 + 4 * i].copy_from_slice(&s.to_le_bytes());
    }
    b
}

/// Encodes the indirect dispatch of the counts at guest address `indirect`. The pipeline and the argument buffer `args`
/// of the dispatch are bound by the caller's next commands, so this binds them itself after the size kernel ran.
pub(super) fn encode(
    sh: &Shared<'_>,
    cp: &mut ComputePass<'_>,
    scratch: &Scratch,
    p: &StageProgram,
    pipeline: nxkit_metal::ComputePipeline,
    args: u64,
    indirect: u64,
) -> Result<(), Skip> {
    let rec = crate::geometry::indirect_args(sh.pools, indirect, 12)?.real;
    let kernel = sh
        .pipelines
        .host_compute("nxkit-dispatch-size", MSL, "nxkit_dispatch_size")?;
    let job = scratch
        .push(&job_bytes(args, rec, &drv_slots(p)))
        .map_err(|e| crate::buf_skip(format_args!("indirect dispatch job: {e:?}")))?;
    let enc = |cp: &mut ComputePass<'_>, c| cp.encode(&c).map_err(|_| Skip::Encode);
    // Dispatches of one pass overlap and passes overlap too: the counts come from an earlier dispatch or pass, so wait
    // for all earlier work, then for the size kernel.
    enc(
        cp,
        ComputeCommand::Depend(Dependency {
            scope: Scope::FromEarlierPasses,
            after: Stages::ALL,
            before: Stages::DISPATCH | Stages::BLIT,
            visibility: Visibility::Device,
        }),
    )?;
    enc(cp, ComputeCommand::Barrier)?;
    enc(cp, ComputeCommand::SetPipeline(kernel))?;
    enc(
        cp,
        ComputeCommand::SetBuffer {
            index: 0,
            gpu_addr: job,
        },
    )?;
    enc(
        cp,
        ComputeCommand::DispatchThreadgroups {
            groups: [1; 3],
            threads: [1; 3],
        },
    )?;
    enc(cp, ComputeCommand::Barrier)?;
    enc(cp, ComputeCommand::SetPipeline(pipeline))?;
    enc(
        cp,
        ComputeCommand::SetBuffer {
            index: 0,
            gpu_addr: args,
        },
    )?;
    enc(
        cp,
        ComputeCommand::DispatchThreadgroupsIndirect {
            gpu_addr: rec,
            threads: p.threadgroup,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_job_holds_the_addresses_the_offset_and_the_slots() {
        let b = job_bytes(0x1111, 0x2222, &[3, 5]);
        assert_eq!(u64::from_le_bytes(b[0..8].try_into().unwrap()), 0x1111);
        assert_eq!(u64::from_le_bytes(b[8..16].try_into().unwrap()), 0x2222);
        assert_eq!(u32::from_le_bytes(b[16..20].try_into().unwrap()), 2);
        assert_eq!(
            u32::from_le_bytes(b[20..24].try_into().unwrap()) as usize,
            DRV_DISPATCH
        );
        assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), 3);
        assert_eq!(u32::from_le_bytes(b[28..32].try_into().unwrap()), 5);
    }

    #[test]
    fn the_job_takes_at_most_eight_slots() {
        let b = job_bytes(0, 0, &[1; 12]);
        assert_eq!(u32::from_le_bytes(b[16..20].try_into().unwrap()), 8);
    }
}
