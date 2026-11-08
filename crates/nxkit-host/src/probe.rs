//! `nxkit-host probe <run folder>`: the measurements that R18 asks for.
//!
//! R18 names three: a no-copy buffer over reserved memory (5), whether residency wires the reservation (6), and
//! whether the code loads as a Mach-O at fixed addresses (1, 2 and 4). Measurement 3 is the cost of a copy into anonymous memory.
//!
//! The probe maps ranges, looks at what the system gave and frees them again. Run it in a fresh process, because
//! what it measures is the address space of a fresh process. Measurements 5 and 6 reserve the whole guest range and
//! keep it until the process ends, so they come last.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use nxkit_metal::Context;

use crate::error::{HostError, Result};
use crate::layout::{self, Layout};
use crate::macho;
use crate::manifest::Game;
use crate::memory;
use crate::place;
use crate::sys::{self, Dylib, PROT_NONE};

const COPY_RUNS: usize = 5;

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn named_ranges(game: &Game, layout: &Layout) -> Vec<(String, u64, u64)> {
    let mut out = vec![("all guest memory".to_string(), layout.all.start, layout.all.len)];
    for m in &game.modules {
        out.push((format!("module {}", m.name), m.start(), m.end() - m.start()));
    }
    out.push(("main stack".into(), layout.main_stack.start, layout.main_stack.len));
    out.push(("thread-local blocks".into(), layout.tls.start, layout.tls.len));
    out.push(("heap".into(), layout.heap.start, layout.heap.len));
    out.push(("stack region".into(), layout.stack.start, layout.stack.len));
    out.push(("alias".into(), layout.alias.start, layout.alias.len));
    out
}

/// Measurement 1: is the address free in a fresh process? A hint without `MAP_FIXED` never replaces a mapping, so
/// the system returns the hint only when the range is free.
fn free_in_fresh_process(ranges: &[(String, u64, u64)]) -> Vec<bool> {
    println!("1. Are the fixed addresses free in this process (mmap with a hint)?");
    if let Some((name, start, len)) = ranges.first() {
        let end = start.saturating_add(*len);
        let verdict = if end <= layout::SPACE_END { "fits" } else { "does NOT fit" };
        println!("   {name}: ends at {end:#x}, which {verdict} below the 39-bit limit {:#x}", layout::SPACE_END);
    }
    let mut free = Vec::new();
    for (name, start, len) in ranges {
        match sys::map_hint(*start, *len, PROT_NONE) {
            Ok(got) if got == *start => {
                println!("   {name}: {start:#x} + {len:#x} free");
                free.push(true);
                sys::free_range(got, *len);
            }
            Ok(got) => {
                println!("   {name}: {start:#x} + {len:#x} NOT free, the system gave {got:#x}");
                free.push(false);
                sys::free_range(got, *len);
            }
            Err(e) => {
                println!("   {name}: {start:#x} + {len:#x} failed: {e}");
                free.push(false);
            }
        }
    }
    free
}

/// Measurement 2: do `MAP_FIXED` and `mach_vm_allocate` with `VM_FLAGS_FIXED` give the range? Only ranges that
/// measurement 1 found free are tried, because `MAP_FIXED` replaces what is there.
fn fixed_calls(ranges: &[(String, u64, u64)], free: &[bool]) {
    println!("2. Do MAP_FIXED and mach_vm_allocate(VM_FLAGS_FIXED) get the ranges?");
    for ((name, start, len), is_free) in ranges.iter().zip(free) {
        if !is_free {
            println!("   {name}: skipped, the range is not free");
            continue;
        }
        let a = sys::map_fixed(*start, *len);
        sys::free_range(*start, *len);
        let b = sys::mach_allocate_fixed(*start, *len);
        sys::free_range(*start, *len);
        let word = |r: &Result<()>| match r {
            Ok(()) => "ok".to_string(),
            Err(e) => format!("failed: {e}"),
        };
        println!("   {name}: MAP_FIXED {}, mach_vm_allocate {}", word(&a), word(&b));
    }
}

/// Measurement 3: what does a copy into anonymous memory cost for a module of the size of the game?
fn copy_cost(game: &Game) -> Result<()> {
    println!("3. Cost of a copy into anonymous memory (map, copy, mprotect to r-x), {COPY_RUNS} runs per module:");
    for m in &game.modules {
        let span = m.end() - m.start();
        let len = span as usize;
        let source = vec![0xA5u8; len];
        let mut totals = Vec::new();
        for _ in 0..COPY_RUNS {
            let t0 = Instant::now();
            let map = sys::ScratchMap::new(len)?;
            map.fill(&source);
            let t1 = Instant::now();
            map.make_executable()?;
            totals.push((t1 - t0, t0.elapsed()));
        }
        totals.sort_unstable_by_key(|t| t.1);
        let Some(&(copy, all)) = totals.get(totals.len() / 2) else {
            continue;
        };
        println!(
            "   module {}: {:#x} bytes, median copy {:.2} ms, median with mprotect {:.2} ms",
            m.name,
            span,
            ms(copy),
            ms(all)
        );
    }
    Ok(())
}

/// Measurement 4: where does dyld put the segments of `nxgame.dylib` in a fresh process? The build asks for the
/// preferred addresses of `game.json`. dyld may slide the library, and the host handles both (R48).
fn dyld_placement(game: &Game) {
    println!("4. Where does dyld put the segments of {}?", game.dylib.display());
    let path = game.path(&game.dylib);
    if !path.exists() {
        println!("   {} does not exist, skipped", path.display());
        return;
    }
    let result = Dylib::open(&path).and_then(|lib| {
        let read = lib.image().and_then(|img| Ok((img, macho::segments(&img.commands()?)?)));
        lib.close();
        read
    });
    let (img, segs) = match result {
        Ok(r) => r,
        Err(e) => {
            println!("   failed: {e}");
            return;
        }
    };
    println!("   header at {:#x}, slide {:#x}", img.header, img.slide);
    for s in segs.iter().filter(|s| s.name.starts_with("__NX_")) {
        let at = s.vmaddr.wrapping_add_signed(img.slide);
        let verdict = if img.slide == 0 { "at the preferred address" } else { "moved" };
        println!("   {}: preferred {:#x}, loaded {at:#x}, {verdict}", s.name, s.vmaddr);
    }
}

/// The wired memory of the whole system in bytes, from `vm_stat`.
fn wired_bytes() -> Result<u64> {
    let out = Command::new("vm_stat")
        .output()
        .map_err(|e| HostError::App(format!("cannot run vm_stat: {e}")))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let number = |line: &str| -> Option<u64> {
        line.chars()
            .filter(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .ok()
    };
    let page = text.lines().next().and_then(|l| l.split("page size of").nth(1)).and_then(number);
    let wired = text.lines().find(|l| l.starts_with("Pages wired down")).and_then(number);
    match (page, wired) {
        (Some(p), Some(w)) => Ok(p * w),
        _ => Err(HostError::App("cannot read the wired pages from vm_stat".into())),
    }
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// Measurements 5 and 6: reserve the guest range the way the host does (read and write, below the 39-bit limit),
/// wrap the part behind the modules as the no-copy buffer of R3 and see how much memory the system wires for it.
/// The modules are not part of the buffer, because the host maps their code from a file and a residency set makes
/// file-mapped pages unreadable. The guard page of the main stack comes after the buffer, as in the host.
fn buffer_and_residency(game: &Game) -> Result<()> {
    println!("5. No-copy buffer over the memory behind the modules, and 6. wired memory:");
    let before = wired_bytes()?;
    let (res, _, layout) = place::reserve_guest(game)?;
    println!("   reservation {:#x} + {:#x}, read and write", res.base(), res.len());
    let mem = memory::init(layout, res)?;
    let ctx = Context::system_default().map_err(|e| HostError::App(format!("Metal: {e}")))?;
    println!(
        "   device {}, maxBufferLength {:.2} GiB, reservation {:.2} GiB",
        ctx.name(),
        ctx.max_buffer_length() as f64 / (1u64 << 30) as f64,
        mem.res.len() as f64 / (1u64 << 30) as f64
    );
    let gpu = mem.gpu_range();
    println!(
        "   GPU range {:#x} + {:#x} ({:.2} GiB), the modules below it are not GPU visible",
        gpu.start,
        gpu.len,
        gpu.len as f64 / (1u64 << 30) as f64
    );
    let t = Instant::now();
    let wrapped = mem.gpu_memory(&ctx);
    let took = t.elapsed();
    match &wrapped {
        Ok(_) => println!("   newBufferWithBytesNoCopy over the GPU range: ok in {:.2} ms", ms(took)),
        Err(e) => println!("   newBufferWithBytesNoCopy over the GPU range: failed: {e}"),
    }
    mem.seal()?;
    println!("   guard page of the main stack set after the buffer");
    let after = wired_bytes()?;
    println!(
        "   wired memory of the system: {:.1} MiB before, {:.1} MiB after, resident allocations {}",
        mib(before),
        mib(after),
        ctx.resident_count()
    );
    Ok(())
}

/// Runs the probe for the game in `root` and prints the results.
pub fn run(root: &Path) -> Result<()> {
    let game = Game::read(root)?;
    let layout = layout::plan(&game, 0)?;
    println!("nxkit-host probe for {}", root.display());
    let ranges = named_ranges(&game, &layout);
    let free = free_in_fresh_process(&ranges);
    fixed_calls(&ranges, &free);
    copy_cost(&game)?;
    dyld_placement(&game);
    buffer_and_residency(&game)
}
