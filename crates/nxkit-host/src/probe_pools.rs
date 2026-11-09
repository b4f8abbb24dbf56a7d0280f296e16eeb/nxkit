//! Probe step 7: what does a no-copy buffer wire, and what does it cost to make and drop buffers at the rate of the
//! game (`docs/plans/gpu-buffer-per-pool.md`, P0)?
//!
//! The runtime wraps the whole guest range as one buffer and the driver wires every page of it. The plan wraps one
//! buffer per memory pool instead. This step reserves 2 GiB, touches every page, wraps three buffers (two that share
//! one page and a big one) and prints the wired memory after each change: with the buffers, after dropping the big
//! one, and after dropping the rest. Then it makes and drops buffers of 64 KiB, as a game does with its pools.
//!
//! `GuestMemory` commits the residency set itself when it makes and when it drops a buffer, as it does in the runtime.
//! So the churn commits once per buffer here, not once per 50.

use std::time::{Duration, Instant};

use nxkit_metal::{Context, GuestMemory};

use crate::error::{HostError, Result};
use crate::layout::PAGE;
use crate::sys::{self, Reservation, paging, vm};

const MIB: u64 = 1024 * 1024;
const RESERVE: u64 = 2048 * MIB;
/// Buffers of the churn case, their size and how many are made between two explicit commits.
const CHURN_BUFFERS: u64 = 1000;
const CHURN_SIZE: u64 = 64 * 1024;
const CHURN_BATCH: u64 = 50;
/// How long the probe waits for the pages of a dropped buffer to be unwired.
const UNWIRE_LIMIT: Duration = Duration::from_secs(2);

fn mib(bytes: u64) -> f64 {
    bytes as f64 / MIB as f64
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// The wired memory of the whole system in bytes. The process has no count of its own: the kernel names wired pages
/// per page, which the page query reads over a range.
fn system_wired() -> u64 {
    paging::system_vm(PAGE).map_or(0, |s| s.wired)
}

/// A range of the reservation that a buffer covers, as an offset from the reservation base.
#[derive(Clone, Copy)]
struct Span {
    name: &'static str,
    off: u64,
    len: u64,
}

/// Prints the wired memory: the system's change since the start, the pages of the whole reservation, and the pages of
/// each named range.
fn report(what: &str, res: &Reservation, base_wired: u64, spans: &[Span]) {
    let whole = vm::page_query(res.base(), res.base() + res.len(), PAGE).unwrap_or_default();
    let delta = system_wired() as i64 - base_wired as i64;
    println!(
        "   {what}: system wired changed by {:+.1} MiB since the start; of the {:.0} MiB reservation \
         {:.1} MiB are resident and {:.1} MiB are wired",
        delta as f64 / MIB as f64,
        mib(res.len()),
        mib(whole.resident),
        mib(whole.wired)
    );
    for s in spans {
        let at = res.base() + s.off;
        let p = vm::page_query(at, at + s.len, PAGE).unwrap_or_default();
        println!(
            "      range {} ({:.2} MiB at +{:.2} MiB): {:.2} MiB wired",
            s.name,
            mib(s.len),
            mib(s.off),
            mib(p.wired)
        );
    }
}

/// Writes one word into every page, so that the system gives each one memory.
fn touch(res: &Reservation) -> Result<()> {
    let mut at = res.base();
    while at < res.base() + res.len() {
        res.write_u64(at, at)?;
        at += PAGE;
    }
    Ok(())
}

fn wrap(res: &Reservation, ctx: &Context, s: Span) -> Result<GuestMemory> {
    res.as_gpu_memory(ctx, res.base() + s.off, s.len)
}

/// The wired bytes of a range of the reservation, from the page query.
fn wired_in(res: &Reservation, off: u64, len: u64) -> u64 {
    let at = res.base() + off;
    vm::page_query(at, at + len, PAGE).map_or(0, |p| p.wired)
}

/// Polls the wired bytes of a range until they are 0, sleeping 0.1 ms at first and then longer, for 2 s at most.
/// Returns the time it took, or the wired bytes that were left after 2 s.
fn wait_unwired(res: &Reservation, off: u64, len: u64) -> std::result::Result<Duration, u64> {
    let start = Instant::now();
    let mut nap = Duration::from_micros(100);
    loop {
        let left = wired_in(res, off, len);
        if left == 0 {
            return Ok(start.elapsed());
        }
        if start.elapsed() >= UNWIRE_LIMIT {
            return Err(left);
        }
        std::thread::sleep(nap);
        nap = (nap * 3 / 2).min(Duration::from_millis(100));
    }
}

/// Prints how long the pages of a dropped range took to unwire.
fn print_unwired(what: &str, res: &Reservation, off: u64, len: u64) {
    match wait_unwired(res, off, len) {
        Ok(d) if d.is_zero() => {
            println!("   {what}: the range was already unwired at the first look")
        }
        Ok(d) => println!(
            "   {what}: the range was unwired {:.1} ms after the drop",
            ms(d)
        ),
        Err(left) => println!(
            "   {what}: still {:.2} MiB wired after {} s of polling",
            mib(left),
            UNWIRE_LIMIT.as_secs()
        ),
    }
}

/// How the dropped buffer of one sub-case is helped, or not, to unwire its pages.
#[derive(Clone, Copy)]
enum Kick {
    /// No Metal call after the drop: only time passes. (The drop itself commits, as in the runtime.)
    TimeOnly,
    /// One more `commit_residency()` right after the drop, no sleep.
    Commit,
    /// A small buffer is made and dropped right after the drop, no sleep.
    SmallBuffer,
    /// The drop happens inside an autorelease pool that is drained before the first look.
    Pool,
}

/// A fresh 64 MiB buffer is made at `off`, dropped with `kick`, and the pages are looked at at once and then polled.
fn unwire_case(res: &Reservation, ctx: &Context, label: &str, kick: Kick, off: u64) -> Result<()> {
    let len = 64 * MIB;
    let buffer = wrap(
        res,
        ctx,
        Span {
            name: "fresh",
            off,
            len,
        },
    )?;
    let live = wired_in(res, off, len);
    match kick {
        Kick::TimeOnly => drop(buffer),
        Kick::Commit => {
            drop(buffer);
            ctx.commit_residency();
        }
        Kick::SmallBuffer => {
            drop(buffer);
            let small = Span {
                name: "small",
                off: 1900 * MIB,
                len: CHURN_SIZE,
            };
            drop(wrap(res, ctx, small)?);
        }
        Kick::Pool => nxkit_metal::autorelease_pool(|| drop(buffer)),
    }
    let t = Instant::now();
    let now = wired_in(res, off, len);
    println!(
        "   unwire case {label}: 64 MiB buffer had {:.2} MiB wired while resident; right after the drop \
         {:.2} MiB were still wired (looked at after {:.2} ms)",
        mib(live),
        mib(now),
        ms(t.elapsed())
    );
    if now > 0 {
        print_unwired(&format!("unwire case {label}"), res, off, len);
    }
    Ok(())
}

/// The three buffers, the two drops, the unwire sub-cases and the churns. Every buffer is gone when it returns.
fn measure(res: &Reservation, ctx: &Context, base_wired: u64) -> Result<()> {
    let a = Span {
        name: "A",
        off: 0,
        len: MIB,
    };
    // B starts one page before the end of A, so the two share that page.
    let b = Span {
        name: "B (overlaps A by one 16 KiB page)",
        off: MIB - PAGE,
        len: MIB,
    };
    let c = Span {
        name: "C",
        off: 4 * MIB,
        len: 64 * MIB,
    };
    let spans = [a, b, c];

    println!("   making the buffers A (1 MiB), B (1 MiB, shares a page with A) and C (64 MiB):");
    let mut buffers = Vec::new();
    for s in spans {
        let t = Instant::now();
        buffers.push(wrap(res, ctx, s)?);
        println!(
            "      buffer {}: made and committed in {:.3} ms",
            s.name,
            ms(t.elapsed())
        );
    }
    let t = Instant::now();
    ctx.commit_residency();
    println!(
        "   commit with nothing pending: {:.3} ms, {} allocations resident",
        ms(t.elapsed()),
        ctx.resident_count()
    );
    report("with A, B and C resident", res, base_wired, &spans);

    let t = Instant::now();
    buffers.pop();
    ctx.commit_residency();
    println!("   dropped C and committed in {:.3} ms", ms(t.elapsed()));
    report("right after dropping C", res, base_wired, &spans);
    print_unwired("after dropping C", res, c.off, c.len);

    let t = Instant::now();
    buffers.clear();
    ctx.commit_residency();
    println!(
        "   dropped A and B and committed in {:.3} ms, {} allocations resident",
        ms(t.elapsed()),
        ctx.resident_count()
    );
    report("right after dropping A and B", res, base_wired, &spans);
    print_unwired("after dropping A and B", res, 0, 2 * MIB - PAGE);

    println!(
        "   what unwires the pages of a dropped buffer? Each case uses a fresh 64 MiB buffer:"
    );
    let cases = [
        ("a (only time passes)", Kick::TimeOnly),
        ("b1 (another commit_residency, no sleep)", Kick::Commit),
        (
            "b2 (a small buffer made and dropped, no sleep)",
            Kick::SmallBuffer,
        ),
        ("c (drop inside an autorelease pool, drained)", Kick::Pool),
    ];
    for (k, (label, kick)) in cases.into_iter().enumerate() {
        unwire_case(res, ctx, label, kick, 256 * MIB + k as u64 * 64 * MIB)?;
    }

    churn(res, ctx, base_wired)?;
    churn_big(res, ctx)
}

/// Makes and drops buffers of 64 KiB over distinct ranges, `CHURN_BATCH` at a time, as a game makes and finalizes
/// its pools.
fn churn(res: &Reservation, ctx: &Context, base_wired: u64) -> Result<()> {
    println!(
        "   churn: making and dropping {CHURN_BUFFERS} buffers of {} KiB, {CHURN_BATCH} at a time:",
        CHURN_SIZE / 1024
    );
    let first = 128 * MIB;
    let all = CHURN_BUFFERS * CHURN_SIZE;
    let (mut make, mut drop_time, mut sampling) = (Duration::ZERO, Duration::ZERO, Duration::ZERO);
    let (mut peak_live, mut peak_dropped) = (0, 0);
    let mut live: Vec<GuestMemory> = Vec::new();
    let t0 = Instant::now();
    for i in 0..CHURN_BUFFERS {
        let s = Span {
            name: "churn",
            off: first + i * CHURN_SIZE,
            len: CHURN_SIZE,
        };
        let t = Instant::now();
        live.push(wrap(res, ctx, s)?);
        make += t.elapsed();
        if (i + 1) % CHURN_BATCH == 0 {
            let t = Instant::now();
            peak_live = peak_live.max(wired_in(res, first, all));
            sampling += t.elapsed();
            let t = Instant::now();
            live.clear();
            ctx.commit_residency();
            drop_time += t.elapsed();
            let t = Instant::now();
            peak_dropped = peak_dropped.max(wired_in(res, first, all));
            sampling += t.elapsed();
        }
    }
    let total = t0.elapsed() - sampling;
    println!(
        "      total {:.1} ms for {CHURN_BUFFERS} buffers (without the wired samples): {:.1} us per buffer to make \
         (with its commit), {:.1} us per buffer to drop (with its commit)",
        ms(total),
        make.as_secs_f64() * 1e6 / CHURN_BUFFERS as f64,
        drop_time.as_secs_f64() * 1e6 / CHURN_BUFFERS as f64
    );
    println!(
        "      at 50 made and 25 dropped per second (the game) this is {:.2} ms of every second",
        (make.as_secs_f64() * 50.0 + drop_time.as_secs_f64() * 25.0) * 1e3 / CHURN_BUFFERS as f64
    );
    println!(
        "      peak wired over the churn ranges, sampled after each batch of {CHURN_BATCH}: {:.2} MiB with the batch \
         still alive, {:.2} MiB right after the batch was dropped and committed (dropped but still wired)",
        mib(peak_live),
        mib(peak_dropped)
    );
    let span = Span {
        name: "the churn ranges",
        off: first,
        len: all,
    };
    report("right after the churn", res, base_wired, &[span]);
    print_unwired("after the churn", res, first, all);
    Ok(())
}

/// The worst case of the size mix: 20 buffers of 64 MiB, one after another, over 8 slots of the reservation that are
/// used again.
fn churn_big(res: &Reservation, ctx: &Context) -> Result<()> {
    const COUNT: u64 = 20;
    const SLOTS: u64 = 8;
    let len = 64 * MIB;
    let first = 1024 * MIB;
    println!(
        "   big churn: making and dropping {COUNT} buffers of 64 MiB one after another, over {SLOTS} slots that are used again:"
    );
    let mut peak = 0;
    for i in 0..COUNT {
        let off = first + (i % SLOTS) * len;
        let s = Span {
            name: "big",
            off,
            len,
        };
        let t = Instant::now();
        let buffer = wrap(res, ctx, s)?;
        let made = t.elapsed();
        let t = Instant::now();
        drop(buffer);
        ctx.commit_residency();
        let dropped = t.elapsed();
        let wired = wired_in(res, first, SLOTS * len);
        peak = peak.max(wired);
        println!(
            "      buffer {:2}: made in {:.2} ms, dropped in {:.2} ms; wired over the {SLOTS} slots after the drop {:.1} MiB",
            i + 1,
            ms(made),
            ms(dropped),
            mib(wired)
        );
    }
    println!(
        "      peak wired over the {SLOTS} slots ({} MiB of address space), counting dropped buffers: {:.1} MiB",
        SLOTS * len / MIB,
        mib(peak)
    );
    print_unwired("after the big churn", res, first, SLOTS * len);
    Ok(())
}

/// Runs step 7. The reservation is unmapped when it returns.
pub fn run() -> Result<()> {
    println!(
        "7. Does a resident no-copy buffer wire only its own pages, and does dropping it unwire them?"
    );
    let ctx = Context::system_default().map_err(|e| HostError::App(format!("Metal: {e}")))?;
    let base_wired = system_wired();
    println!(
        "   system wired memory at the start: {:.1} MiB; page size {} KiB",
        mib(base_wired),
        PAGE / 1024
    );
    let res = sys::reserve_where(&[0], RESERVE, |_| true)?;
    println!(
        "   reserved {:.0} MiB read and write at {:#x}",
        mib(res.len()),
        res.base()
    );
    let t = Instant::now();
    let touched = touch(&res);
    println!(
        "   touched every page in {:.0} ms (no buffer yet)",
        ms(t.elapsed())
    );
    let result = touched.and_then(|()| {
        report("reservation touched, no buffer", &res, base_wired, &[]);
        measure(&res, &ctx, base_wired)
    });
    let unmapped = res.unmap(res.base(), res.len());
    println!(
        "   reservation unmapped, system wired memory {:+.1} MiB from the start",
        (system_wired() as i64 - base_wired as i64) as f64 / MIB as f64
    );
    result.and(unmapped)
}
