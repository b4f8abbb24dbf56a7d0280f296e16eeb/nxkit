//! The copy-in pass: the jobs that move staged pages into the mirror at the start of a frame.

use super::*;

/// One page of the copy into the mirror: the mirror address, the staging address, and the 64-byte blocks to leave alone
/// (bit `b % 64` of word `b / 64`; they hold what a frame in flight writes).
pub(crate) type CopyJob = (u64, u64, [u64; 4]);

/// The kernel that copies staged pages into the mirror: one page per 256 threads, 16 bytes each, four times, except the
/// blocks of 64 bytes that the job skips.
const COPY_IN_MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;
struct page_job {
    device uint4 *dst;
    device const uint4 *src;
    ulong skip[4];
};
kernel void nxkit_private_in(device const page_job *jobs [[buffer(0)]], uint t [[thread_position_in_grid]]) {
    page_job j = jobs[t >> 8];
    uint lane = t & 255u;
    for (uint i = lane; i < 1024u; i += 256u) {
        uint b = i >> 2;
        if (((j.skip[b >> 6] >> (b & 63u)) & 1ul) != 0ul) continue;
        j.dst[i] = j.src[i];
    }
}
"#;

/// The [`CopyJob`] of every page that the frame's first command buffer copies, and the number of
/// staged pages whose page is no longer a member of the view (they are dropped).
pub(crate) fn copy_in_jobs(o: &GuestOverlay) -> (Vec<CopyJob>, u32) {
    let Some(st) = o.staged() else {
        return (Vec::new(), 0);
    };
    let base = st.buf.gpu_address();
    let mut lost = 0;
    let jobs = st
        .slots
        .iter()
        .filter_map(|&(page, slot)| {
            let va = o.va_base() + u64::from(page) * PAGE;
            match o.locate(va) {
                Some((_, real, _)) => Some((
                    real,
                    base + u64::from(slot) * PAGE,
                    st.skip_of(page).unwrap_or([0; 4]),
                )),
                None => {
                    lost += 1;
                    None
                }
            }
        })
        .collect();
    (jobs, lost)
}

/// Encodes the copy of the staged pages into the mirror as a compute pass of `cb`, with a barrier from all earlier work
/// (the frame before still reads and writes the mirror) and one for all later work. Returns the pages copied.
pub(crate) fn encode_copy_in(
    pipelines: &Pipelines,
    cb: &mut CommandBuffer,
    table: &ArgumentTable,
    scratch: &Scratch,
    jobs: &[CopyJob],
) -> Result<u32, Skip> {
    if jobs.is_empty() {
        return Ok(0);
    }
    let bytes: Vec<u8> = jobs
        .iter()
        .flat_map(|&(dst, src, skip)| {
            dst.to_le_bytes()
                .into_iter()
                .chain(src.to_le_bytes())
                .chain(skip.into_iter().flat_map(u64::to_le_bytes))
        })
        .collect();
    let job = scratch.push(&bytes).map_err(|_| Skip::Buffer)?;
    let pipeline = pipelines.host_compute("nxkit-private", COPY_IN_MSL, "nxkit_private_in")?;
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
            grid: [256 * jobs.len() as u32, 1, 1],
            threads: [256, 1, 1],
        },
    )?;
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
    Ok(jobs.len() as u32)
}

/// Results one frame late: the copies of the frame's late pages from the mirror into the buffer of [`LateCopies`], at
/// the start of the frame (after the copy-in) and at its end (after the last pass), and the host addresses of the two
/// copies. None when the frame has none, or when a page has no place in the mirror (the completion then drops the
/// snapshots that need the copies).
pub(crate) fn late_copy_jobs(o: &GuestOverlay) -> Option<(Vec<CopyJob>, Vec<CopyJob>, u64, u64)> {
    let l = o.late()?;
    let n = l.pages.len() as u64;
    let (gpu, host) = (l.buf.gpu_address(), l.buf.host_address());
    let mut start = Vec::with_capacity(l.pages.len());
    let mut end = Vec::with_capacity(l.pages.len());
    for (i, &page) in l.pages.iter().enumerate() {
        let (_, real, room) = o.locate(o.va_base() + u64::from(page) * PAGE)?;
        if room < PAGE {
            return None;
        }
        start.push((gpu + i as u64 * PAGE, real, [0; 4]));
        end.push((gpu + (n + i as u64) * PAGE, real, [0; 4]));
    }
    Some((start, end, host, host + n * PAGE))
}

/// The copies of [`late_copy_jobs`] are encoded into the frame `frame`: the completion of the frame takes the frame's
/// bytes from them. Call it before the commit.
pub(crate) fn late_copies_encoded(frame: u64, start: u64, end: u64) {
    Pages::global().set_late_copies(frame, start, end);
}

/// Counts what the copy-in of a frame did, for the report.
pub(crate) fn note_copy_in(pages: u32, lost: u32) {
    let mut w = locked(&state().window);
    w.copy_in_pages += u64::from(pages);
    w.copy_in_lost += u64::from(lost);
}

/// True when a page of the guest GPU range `[addr, addr + len)` may hold stale guest bytes because a frame wrote it on the
/// GPU (the walker does not snapshot such a range from guest memory).
pub(crate) fn gpu_owned(host_addr: u64, len: u64) -> bool {
    mirrored() && Pages::global().any_gpu_owned(host_addr, len)
}

/// True when every page of the guest range is a member, so a frame that binds it reads the mirror (the walk takes no
/// snapshot of the range).
pub(crate) fn mirror_covers(host_addr: u64, len: u64) -> bool {
    mirrored() && Pages::global().all_members(host_addr, len)
}
