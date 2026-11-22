//! The textures of virtual memory pools: their bytes are gathered through the page table of the flush.
//!
//! A virtual pool that the game maps page by page (a sparse pool) has guest GPU addresses of its own, in the window
//! that [`virtual_window`] gives the front end. No buffer covers that window: the GPU of the host sees guest memory
//! only through the no-copy buffers of the physical pools, and the host cannot remap pages under such a buffer
//! (`docs/nxkit-host.md`, "Guest memory"). So the upload of such a texture copies its bytes on the CPU from the
//! physical pages that the page table names into a buffer of its own (an unmapped page reads as 0), and deswizzles
//! from there. The residency of the record says when: a mapping request in the range of the texture or a CPU write
//! to a physical page behind it makes the record `GuestNewer` (`FrontEnd::virtual_changed`), as a flush does for a
//! texture of a physical pool.

use std::sync::Arc;

use nxkit_metal::{Context, GUEST_VA_MAX, GuestMemory};
use nxkit_nvn::resource::TextureRecord;
use nxkit_nvn::vspace::{Gathered, PageTables};

use crate::guestpools::GuestPools;
use crate::skip::Skip;

/// The length of the window of the virtual pools, 128 GiB at the top of the window of guest GPU addresses.
pub(crate) const VIRTUAL_WINDOW_LEN: u64 = 0x20_0000_0000;

/// The window of the virtual pools: the top [`VIRTUAL_WINDOW_LEN`] bytes below `GUEST_VA_MAX` (`1 << 40`). The guest
/// memory takes the window of guest addresses from its start, `gpu_base`, so the two never meet while the guest
/// memory is less than 128 GiB long. Metal never hands out an address in either (`check_outside_window`). None when the
/// guest memory reaches into it.
pub(crate) fn virtual_window(gpu_base: u64, guest_len: u64) -> Option<(u64, u64)> {
    let start = GUEST_VA_MAX - VIRTUAL_WINDOW_LEN;
    if gpu_base.saturating_add(guest_len) > start {
        eprintln!(
            "nxkit-gpu: warning: the guest memory {gpu_base:#x}+{guest_len:#x} reaches into the window of the virtual memory pools at {start:#x}; virtual pools get no addresses of their own"
        );
        return None;
    }
    Some((start, VIRTUAL_WINDOW_LEN))
}

/// The page tables that hold the texture of `rec`, None when it lies in no virtual pool.
pub(super) fn tables_of<'p>(rec: &TextureRecord, pools: &'p GuestPools) -> Option<&'p PageTables> {
    pools.vpools().filter(|t| t.contains(rec.address.get()))
}

/// Gathers the bytes of the texture of `rec` through `tables` into a new buffer, with the physical bytes read from
/// the pool buffers of `pools`.
pub(super) fn gather(
    ctx: &Context,
    tables: &PageTables,
    rec: &TextureRecord,
    pools: &GuestPools,
) -> Result<Arc<GuestMemory>, Skip> {
    let size = rec.layout.size;
    let mut bytes = vec![0u8; usize::try_from(size).map_err(|_| Skip::Transfer)?];
    let g = tables
        .gather(rec.address, &mut bytes, |at, out| read(pools, at, out))
        .map_err(|_| Skip::Transfer)?;
    note(rec, g);
    let mem = GuestMemory::allocate(ctx, size.max(1)).map_err(|_| Skip::Transfer)?;
    mem.write(0, &bytes).map_err(|_| Skip::Transfer)?;
    Ok(Arc::new(mem))
}

/// Reads guest memory at the guest GPU address `at` from the pool buffers. A run that crosses the end of a pool is
/// read page by page.
fn read(pools: &GuestPools, at: u64, out: &mut [u8]) -> bool {
    if let Ok((mem, off)) = crate::pools::locate(pools, at, out.len() as u64) {
        return mem.read(off, out).is_ok();
    }
    let mut ok = true;
    let mut done = 0usize;
    while done < out.len() {
        let a = at + done as u64;
        let n = ((nxkit_nvn::vspace::VPAGE - a % nxkit_nvn::vspace::VPAGE) as usize)
            .min(out.len() - done);
        let chunk = &mut out[done..done + n];
        match crate::pools::locate(pools, a, n as u64) {
            Ok((mem, off)) if mem.read(off, chunk).is_ok() => {}
            _ => {
                chunk.fill(0);
                ok = false;
            }
        }
        done += n;
    }
    ok
}

/// Counts mapped bytes that no pool holds for the health report, and prints every sparse upload under
/// `NXKIT_DIAG=upload` (the upload loop prints the first 200 uploads of every kind as well).
fn note(rec: &TextureRecord, g: Gathered) {
    if g.missing > 0 {
        nxkit_nvn::diag::note(
            nxkit_nvn::diag::Reason::UploadFailed,
            format_args!(
                "sparse {:?} at {:#x}: {:#x} mapped bytes in no pool",
                rec.id,
                rec.address.get(),
                g.missing
            ),
        );
    }
    if crate::diag("upload") {
        eprintln!(
            "DIAG upload sparse {:?} {:?} {}x{} at {:#x}: mapped {:#x} unmapped {:#x} missing {:#x} in {} reads",
            rec.id,
            rec.desc.format,
            rec.desc.width,
            rec.desc.height,
            rec.address.get(),
            g.mapped,
            g.unmapped,
            g.missing,
            g.reads
        );
    }
}
