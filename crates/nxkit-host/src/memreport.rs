//! The memory report: `NXKIT_MEM_REPORT=<seconds>` prints where the memory of the process is, every that many seconds,
//! on stderr. One line starts with `nxkit-mem`, so a run log can be grepped for it.
//!
//! The parts:
//! - `footprint`: `phys_footprint`, what Activity Monitor shows and what the system swaps for. `resident` is in RAM,
//!   `compressed` sits in the compressor or swap, `graphics` is GPU driver memory charged to the process (private
//!   textures, buffers, pipeline code).
//! - `guest`: the reservation of the game. `mapped` is the address space, `dirty` what the game wrote, `compressed`
//!   what of it the system compressed or swapped. Only touched pages cost memory. The numbers come from a query of
//!   every page (`vm::page_query`). The numbers `by region` come from the region walk, which counts by VM object and
//!   overstates this range (12.6 GiB next to a footprint of 113 MiB), so do not trust them.
//! - `malloc`, `ioaccel`, `iosurface`, `files`, `other`: the other regions, by their VM tag. `files` is clean,
//!   file-backed memory (the pipeline archive, metallibs) that the system drops under pressure without writing it out.
//! - `metal`: `currentAllocatedSize` of the device, split into the no-copy guest buffer (its whole length counts,
//!   touched or not, which is what the Metal HUD shows), the textures and buffers of `nxkit-metal`, and the rest
//!   (pipeline code, command allocators, ICBs, drawables).
//! - `libs` and `pipelines`: live shader libraries and the pipeline states the caches hold.

use std::time::{Duration, Instant};

use nxkit_metal::Context;

use crate::layout::PAGE;
use crate::sys::vm;

const MIB: u64 = 1024 * 1024;

/// The seconds between two reports, from `NXKIT_MEM_REPORT`. None when it is unset, 0 or not a number.
fn interval() -> Option<Duration> {
    let v = std::env::var("NXKIT_MEM_REPORT").ok()?;
    let s: f64 = v.trim().parse().ok()?;
    (s > 0.0).then(|| Duration::from_secs_f64(s.max(0.5)))
}

/// Starts the report thread when `NXKIT_MEM_REPORT` asks for it. `guest` is the guest reservation as
/// `(start, end)` in host addresses.
pub fn start(ctx: &Context, guest: (u64, u64)) {
    let Some(every) = interval() else {
        return;
    };
    let ctx = ctx.clone();
    let t0 = Instant::now();
    let spawned = std::thread::Builder::new()
        .name("nxkit-mem".into())
        .spawn(move || {
            loop {
                eprintln!("{}", line(&ctx, guest, t0.elapsed()));
                std::thread::sleep(every);
            }
        });
    if let Err(e) = spawned {
        eprintln!("nxkit-mem: the report thread did not start: {e}");
    }
}

fn mib(v: u64) -> u64 {
    v / MIB
}

fn pages(name: &str, p: vm::Pages) -> String {
    format!(
        "{name} res {} dirty {} comp {}",
        mib(p.resident),
        mib(p.dirty),
        mib(p.swapped)
    )
}

/// One report line. All sizes are MiB.
fn line(ctx: &Context, (g0, g1): (u64, u64), at: Duration) -> String {
    let walk_start = Instant::now();
    let task = vm::task_memory().unwrap_or_default();
    let r = vm::regions(g0, g1, PAGE);
    let guest = vm::page_query(g0, g1, PAGE).unwrap_or(r.guest);
    let walk_ms = walk_start.elapsed().as_millis();
    let m = nxkit_metal::mem_stats();
    let device = ctx.allocated_bytes();
    let rest = device
        .saturating_sub(m.guest_bytes)
        .saturating_sub(m.texture_bytes)
        .saturating_sub(m.buffer_bytes);
    format!(
        "nxkit-mem t {}s (MiB): footprint {} peak {} = resident {} (anon {} file {}) compressed {} graphics {} (comp {}) | \
         guest mapped {} {} (by region: res {} dirty {}) | {} in use {} | {} | {} | {} | {} | {} | \
         metal device {} = guest buffer {} + textures {} (n {}) + buffers {} (n {}) + rest {} | libs {} pipelines {} resident set {} | walk {walk_ms} ms",
        at.as_secs(),
        mib(task.footprint),
        mib(task.footprint_peak),
        mib(task.resident),
        mib(task.internal),
        mib(task.external),
        mib(task.compressed),
        mib(task.graphics),
        mib(task.graphics_compressed),
        mib(guest.mapped),
        pages("", guest).trim_start(),
        mib(r.guest.resident),
        mib(r.guest.dirty),
        pages("malloc", r.malloc),
        mib(vm::malloc_in_use()),
        pages("ioaccel", r.ioaccel),
        pages("iosurface", r.iosurface),
        pages("iokit", r.iokit),
        pages("files", r.files),
        pages("other", r.other),
        mib(device),
        mib(m.guest_bytes),
        mib(m.texture_bytes),
        m.textures,
        mib(m.buffer_bytes),
        m.buffers,
        mib(rest),
        m.libraries,
        m.pipelines,
        ctx.resident_count(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_names_every_part() {
        let Ok(ctx) = Context::system_default() else {
            return;
        };
        let l = line(&ctx, (0, 0), Duration::from_secs(3));
        for part in [
            "nxkit-mem t 3s",
            "footprint",
            "graphics",
            "guest mapped 0",
            "malloc res",
            "ioaccel res",
            "files res",
            "metal device",
            "pipelines",
        ] {
            assert!(l.contains(part), "{part} missing in {l}");
        }
    }
}
