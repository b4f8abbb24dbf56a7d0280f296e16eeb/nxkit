//! The memory report: `NXKIT_MEM_REPORT=<seconds>` prints where the memory of the process is, every that many seconds,
//! on stderr. The lines start with `nxkit-mem`, so a run log can be grepped for them.
//!
//! The parts:
//! - `footprint`: `phys_footprint`, what Activity Monitor shows and what the system swaps for. `resident` is in RAM,
//!   `compressed` sits in the compressor or swap, `graphics` is GPU driver memory charged to the process (private
//!   textures, buffers, pipeline code).
//! - `guest`: the range of the no-copy GPU buffer. `mapped` is the address space, `res` in RAM, `dirty` written, `comp`
//!   compressed or swapped, `wired` wired. While the buffer is resident the GPU driver wires all of it, touched or not,
//!   and none of it counts in `footprint` (measured 2026-11-09), so `res` and `wired` near `mapped` are real RAM. The
//!   numbers come from a query of every page (`vm::page_query`); `by region` is the region walk, by VM object.
//! - `malloc`, `ioaccel`, `iosurface`, `files`, `other`: the other regions, by their VM tag. `files` is clean,
//!   file-backed memory (the pipeline archive, metallibs) that the system drops under pressure without writing it out.
//! - `metal`: `currentAllocatedSize` of the device, split into the no-copy guest buffer (its whole length counts,
//!   touched or not, which is what the Metal HUD shows), the textures and buffers of `nxkit-metal`, and the rest
//!   (pipeline code, command allocators, ICBs, drawables).
//! - `libs` and `pipelines`: live shader libraries and the pipeline states the caches hold.
//! - `tex evicted`: the Metal textures the encoder dropped for being idle (`NXKIT_TEX_EVICT`) with their bytes, and
//!   `reuploaded`: how many of them were made and uploaded again when the game used them later.
//!
//! The third line, `nxkit-mem regions`, is [`region_use`]: how much of the heap, alias and stack regions the game used
//! at most. The host prints it at the end of every run as well.
//!
//! The fourth line, `nxkit-mem pools`, is [`end_line`] (`poolreport`): the NVN memory pools that are live now, how much
//! guest memory they cover (`pages`: each pool rounded out to 16 KiB pages, overlaps counted once), how it splits into
//! heap, alias and other, and the pools by size. It shows how much of the range one no-copy buffer for each pool
//! would have to reach. The host prints it at the end of every run as well.
//!
//! The second line, `nxkit-mem paging`, has rates per second over the interval since the line before:
//! - `task`: the faults of this process, its page-ins (a page read from a file, the compressor or swap), its
//!   decompressions and swap-ins (`TASK_VM_INFO`). Page-ins and decompressions above a few hundred per second while
//!   the game runs mean that it waits for memory that macOS took away.
//! - `system`: free and wired memory, the compressor's size, and the system's decompressions, compressions, page-ins,
//!   swap-ins and swap-outs per second; `level` is the free percentage that `memory_pressure` prints.

use std::time::{Duration, Instant};

use nxkit_metal::Context;
use nxkit_nvn::pools;

use crate::layout::{Layout, PAGE};
use crate::poolreport::pool_line;
use crate::sys::paging::{self, SystemVm, TaskEvents, per_second};
use crate::sys::vm::{self, TaskMemory};

const MIB: u64 = 1024 * 1024;

/// The seconds between two reports, from `NXKIT_MEM_REPORT`. None when it is unset, 0 or not a number.
fn interval() -> Option<Duration> {
    let v = std::env::var("NXKIT_MEM_REPORT").ok()?;
    let s: f64 = v.trim().parse().ok()?;
    (s > 0.0).then(|| Duration::from_secs_f64(s.max(0.5)))
}

/// Starts the report thread when `NXKIT_MEM_REPORT` asks for it.
pub fn start(ctx: &Context, layout: &Layout) {
    let Some(every) = interval() else {
        return;
    };
    let (layout, guest) = (*layout, (layout.all.start, layout.all.end()));
    let ctx = ctx.clone();
    let t0 = Instant::now();
    let spawned = std::thread::Builder::new()
        .name("nxkit-mem".into())
        .spawn(move || {
            let mut last = Sample::now();
            loop {
                eprintln!("{}", line(&ctx, guest, t0.elapsed()));
                std::thread::sleep(every);
                let now = Sample::now();
                eprintln!("{}", rates(&last, &now, t0.elapsed()));
                eprintln!("nxkit-mem regions {}", region_use(&layout));
                eprintln!("{}", pool_line(&layout, &pools::snapshot()));
                last = now;
            }
        });
    if let Err(e) = spawned {
        eprintln!("nxkit-mem: the report thread did not start: {e}");
    }
}

/// The pool line, for the end of a run.
pub fn end_line(l: &Layout) -> String {
    pool_line(l, &pools::snapshot())
}

fn mib(v: u64) -> u64 {
    v / MIB
}

/// What the game used of its regions (`nxkit_hos::usage`) beside their sizes, in MiB. The GPU buffer covers the
/// regions whole and the driver wires all of it (12.4 GiB for the 12.5 GiB split, measured 2026-11-09), so the gap
/// between a peak and its region is RAM that a smaller `memory` in the title would give back.
pub fn region_use(l: &Layout) -> String {
    let u = nxkit_hos::usage::get();
    format!(
        "(MiB): heap peak {} of {} | MapPhysicalMemory peak {} (now {}), alias top {} of {} | MapMemory stack top {} of {} | GPU buffer {}",
        mib(u.heap_peak),
        mib(l.heap.len),
        mib(u.physical_peak),
        mib(u.physical),
        mib(u.alias_top),
        mib(l.alias.len),
        mib(u.stack_top),
        mib(l.stack.len),
        mib(l.all.end() - l.tls.start),
    )
}

/// The counters that the paging line takes the difference of.
struct Sample {
    at: Instant,
    task: Option<TaskEvents>,
    vm: TaskMemory,
    system: Option<SystemVm>,
}

impl Sample {
    fn now() -> Self {
        Self {
            at: Instant::now(),
            task: paging::task_events(),
            vm: vm::task_memory().unwrap_or_default(),
            system: paging::system_vm(PAGE),
        }
    }
}

/// The paging line: rates per second between two samples. Counts are pages, sizes MiB.
fn rates(a: &Sample, b: &Sample, at: Duration) -> String {
    let secs = b.at.duration_since(a.at).as_secs_f64();
    let wrap32 = 1u64 << 32;
    let mut out = format!("nxkit-mem paging t {}s (per s):", at.as_secs());
    if let (Some(x), Some(y)) = (a.task, b.task) {
        out += &format!(
            " task faults {} pageins {}",
            per_second(x.faults.into(), y.faults.into(), wrap32, secs),
            per_second(x.pageins.into(), y.pageins.into(), wrap32, secs),
        );
    }
    out += &format!(
        " decompressions {} swapins {}",
        per_second(a.vm.decompressions, b.vm.decompressions, wrap32, secs),
        per_second(a.vm.swapins, b.vm.swapins, 0, secs),
    );
    if let (Some(x), Some(y)) = (a.system, b.system) {
        let r = |p: u64, q: u64| per_second(p, q, 0, secs);
        out += &format!(
            " | system free {} wired {} compressor {} level {} | decompressions {} compressions {} pageins {} swapins {} swapouts {}",
            mib(y.free),
            mib(y.wired),
            mib(y.compressor),
            y.level.map_or_else(|| "?".into(), |l| format!("{l}%")),
            r(x.decompressions, y.decompressions),
            r(x.compressions, y.compressions),
            r(x.pageins, y.pageins),
            r(x.swapins, y.swapins),
            r(x.swapouts, y.swapouts),
        );
    }
    out
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
         guest mapped {} {} wired {} (by region: res {} dirty {}) | {} in use {} | {} | {} | {} | {} | {} | \
         metal device {} = guest buffer {} + textures {} (n {}) + buffers {} (n {}) + rest {} | libs {} pipelines {} resident set {} | archive shards {} ({} MiB, opened {}) | tex evicted {} ({} MiB) reuploaded {} | walk {walk_ms} ms",
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
        mib(guest.wired),
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
        m.archive_shards,
        mib(m.archive_shard_bytes),
        m.archive_opens,
        m.evictions,
        mib(m.evicted_bytes),
        m.reuploads,
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
            "archive shards",
            "tex evicted",
        ] {
            assert!(l.contains(part), "{part} missing in {l}");
        }
    }

    #[test]
    fn the_region_line_names_the_regions_and_the_buffer() {
        let r = |start: u64, len: u64| crate::layout::Range { start, len };
        let l = Layout {
            all: r(0, 0x5000_0000),
            modules: r(0, 0x1000_0000),
            main_stack: r(0x3000_0000, 0x10_0000),
            tls: r(0x1000_0000, 0x40_0000),
            shared: r(0x1040_0000, 0x400_0000),
            heap: r(0x1100_0000, 0x1f00_0000),
            stack: r(0x3000_0000, 0x1000_0000),
            alias: r(0x4000_0000, 0x1000_0000),
        };
        let line = region_use(&l);
        for part in [
            "heap peak",
            "of 496",
            "alias top",
            "of 256",
            "stack top",
            "GPU buffer 1024",
        ] {
            assert!(line.contains(part), "{part} missing in {line}");
        }
    }

    #[test]
    fn the_paging_line_has_task_and_system_rates() {
        let a = Sample::now();
        let b = Sample::now();
        let l = rates(&a, &b, Duration::from_secs(7));
        for part in [
            "nxkit-mem paging t 7s",
            "task faults",
            "decompressions",
            "system free",
            "swapouts",
        ] {
            assert!(l.contains(part), "{part} missing in {l}");
        }
    }
}
