//! Tests of late pages, the adaptive settings, the stage pool, the sink, stream extents and page runs.

use super::*;

#[test]
fn a_frame_with_forced_submits_gets_the_pages_the_cpu_writes_until_their_signal() {
    let _t = locked(&TURN);
    let Some(g) = guest(64 << 20) else { return };
    let page = PAGE;
    // Page 90 is a member that frame 40 wrote and finished; frame 41 writes page 80 only.
    let _o40 = at_flush(
        &dispatch(VA + 90 * page, 0x100),
        40,
        &g.map,
        0,
        false,
        &|| {},
    )
    .expect("overlay");
    encoded(40, None);
    late_expect(41, 1);
    let o = at_flush(
        &dispatch(VA + 80 * page, 0x100),
        41,
        &g.map,
        1,
        true,
        &|| {},
    )
    .expect("overlay");
    assert!(o.forced() && o.frame() == 41);
    // The CPU writes page 90 after the flush and before the signal that the forced submit waits for.
    g.mem.write(90 * page + 16, &[5; 8]).unwrap();
    assert_eq!(
        Pages::global().state_of(g.mem.host_base() + 90 * page),
        "dirty"
    );
    late_signal(41);
    assert_eq!(
        Pages::global().state_of(g.mem.host_base() + 90 * page),
        "clean",
        "the capture took the page"
    );
    let t0 = Instant::now();
    let jobs = late_stage(&o);
    assert!(
        t0.elapsed() < std::time::Duration::from_millis(10),
        "the signal came, no wait"
    );
    assert_eq!(
        jobs.len(),
        1,
        "one page goes into the mirror before the commit"
    );
    // A write after the signal is the next frame's: it stays dirty for the next flush.
    g.mem.write(90 * page + 32, &[6; 8]).unwrap();
    assert_eq!(
        Pages::global().state_of(g.mem.host_base() + 90 * page),
        "dirty"
    );
    encoded(41, None);
    FORCE.with(|f| f.set(None));
}

#[test]
fn a_signal_that_comes_before_the_flush_counted_it_closes_the_capture_and_one_that_never_comes_times_out()
 {
    let _t = locked(&TURN);
    let Some(g) = guest(64 << 20) else { return };
    let page = PAGE;
    let _o = at_flush(
        &dispatch(VA + 100 * page, 0x100),
        50,
        &g.map,
        0,
        false,
        &|| {},
    )
    .expect("overlay");
    encoded(50, None);
    // The signal first, the count second.
    late_signal(51);
    late_expect(51, 1);
    let o = at_flush(
        &dispatch(VA + 110 * page, 0x100),
        51,
        &g.map,
        1,
        true,
        &|| {},
    )
    .expect("overlay");
    let t0 = Instant::now();
    assert!(late_stage(&o).is_empty());
    assert!(t0.elapsed() < std::time::Duration::from_millis(10));
    encoded(51, None);
    // No signal: the encode waits for the limit, then goes on, and the miss is counted.
    let missed = LATE_MISSED.load(Ordering::Relaxed);
    late_expect(52, 1);
    let o = at_flush(
        &dispatch(VA + 120 * page, 0x100),
        52,
        &g.map,
        1,
        true,
        &|| {},
    )
    .expect("overlay");
    let t0 = Instant::now();
    assert!(late_stage(&o).is_empty());
    assert!(
        t0.elapsed() >= std::time::Duration::from_millis(10),
        "waited for the signal"
    );
    assert_eq!(LATE_MISSED.load(Ordering::Relaxed), missed + 1);
    LATE_MISSED.store(0, Ordering::Relaxed);
    encoded(52, None);
    FORCE.with(|f| f.set(None));
}

#[test]
fn a_range_that_resolved_partly_on_members_is_made_whole_by_the_next_flush() {
    let _t = locked(&TURN);
    let Some(g) = guest(64 << 20) else { return };
    let page = PAGE;
    // Page 50 is written by the frame and so a member; a texture-like source starts in page 49, which no binding
    // announces, and reaches into page 50.
    let detail = dispatch(VA + 50 * page, 0x100);
    let o1 = at_flush(&detail, 1, &g.map, 0, false, &|| {}).expect("overlay");
    let src = VA + 49 * page + 0x3000;
    let len = 0x2000u64;
    assert!(!o1.all_members(src, len));
    let pools = pools_of(&g, o1);
    let before = crate::overlay::partial_table();
    let _ = pools.resolve_c(src, len, crate::overlay::UPLOAD);
    let after = crate::overlay::partial_table();
    let upload = 4usize;
    assert_eq!(
        after[upload][1] - before[upload][1],
        1,
        "counted as starting out and reaching in"
    );
    encoded(1, None);
    // The next flush has the range from the encoder and makes it whole.
    let o2 = at_flush(&detail, 2, &g.map, 0, false, &|| {}).expect("overlay");
    assert!(
        o2.all_members(src, len),
        "the range is whole on members now"
    );
    let pools = pools_of(&g, o2);
    let before = crate::overlay::partial_table();
    let loc = pools.resolve_c(src, len, crate::overlay::UPLOAD).unwrap();
    assert_ne!(loc.run, crate::pools::NO_RUN);
    assert_eq!(
        crate::overlay::partial_table(),
        before,
        "no partial resolve any more"
    );
    encoded(2, None);
    FORCE.with(|f| f.set(None));
}

/// The live case in small: 71 bindings of 72 pages that every frame may write, 154 pages the CPU writes every frame
/// that no frame touches and 77 pages of the written ones that it reads and writes every frame. The three settings give
/// the same bytes; the times are printed.
#[test]
fn the_adaptive_settings_keep_the_bytes_exact_and_the_numbers_are_printed() {
    let _t = locked(&TURN);
    // The tests before this one left pages of their guest tracked and without access; the tracker still covers them.
    Pages::global().release_all();
    let Some(g) = guest(256 << 20) else { return };
    let page = PAGE;
    let mut dispatches = Vec::new();
    let mut written_pages: Vec<u64> = Vec::new();
    let mut hot: Vec<u64> = Vec::new();
    for i in 0..71u64 {
        let first = 400 + i * 100;
        let mut d = dispatch(VA + first * page, (72 * page) as u32);
        // A uniform that starts in the last written page and reaches 2 or 3 pages past it: those pages are members
        // that no frame writes.
        let extra = if i < 12 { 4 } else { 3 };
        d.dispatches[0].ubos = vec![UboDetail {
            stage: Stage::Compute,
            slot: 0,
            address: VA + (first + 71) * page + 0x100,
            size: (extra * page) as u32,
            snapshot: None,
        }];
        hot.extend((0..extra - 1).map(|k| first + 72 + k));
        dispatches.push(d.dispatches.remove(0));
        written_pages.push(first);
        if i < 6 {
            written_pages.push(first + 1);
        }
    }
    let detail = FrameDetail {
        dispatches,
        ..Default::default()
    };
    let pages = Pages::global();
    let mut fence = 0u64;
    let mut out = Vec::new();
    let mut results: Vec<(bool, bool, u64, u64)> = Vec::new();
    for (name, eager, hot_on) in [
        ("off", false, false),
        ("copy-back", true, false),
        ("copy-back and writable", true, true),
    ] {
        pages.set_adaptive(eager, hot_on);
        let (mut cpu_ns, mut flush_ns) = (0u128, 0u128);
        let frames = 80u64;
        let mut prev = 0u8;
        let s0 = pages.stats();
        for k in 0..frames {
            fence += 1;
            let v = (fence % 251) as u8 + 1;
            let t = Instant::now();
            for &h in &hot {
                g.mem.write(h * page + 100, &[v]).unwrap();
            }
            for &w in &written_pages {
                if k > 0 {
                    let mut b = [0u8; 1];
                    g.mem.read(w * page, &mut b).unwrap();
                    assert_eq!(b[0], prev, "{name}: the GPU's byte of the last frame");
                }
                g.mem.write(w * page + 512, &[v]).unwrap();
            }
            cpu_ns += t.elapsed().as_nanos();
            let t = Instant::now();
            let o = at_flush(&detail, fence, &g.map, 0, false, &|| {}).expect("overlay");
            flush_ns += t.elapsed().as_nanos();
            // The hot pages reach the frame's view with the CPU's byte.
            for &h in hot.iter().step_by(17) {
                let mut b = [0u8; 1];
                assert!(o.read(VA + h * page + 100, &mut b));
                assert_eq!(b[0], v, "{name}: staged or mirrored byte of page {h}");
            }
            // The GPU writes the first byte of every written page.
            for &w in &written_pages {
                let va = VA + w * page;
                let run = o
                    .runs()
                    .iter()
                    .find(|r| r.va_start <= va && va < r.va_end)
                    .expect("run");
                run.mem.write(va - run.va_start, &[v]).unwrap();
            }
            encoded(fence, None);
            prev = v;
            std::thread::sleep(std::time::Duration::from_millis(3));
        }
        let s1 = pages.stats();
        eprintln!("--- {name}");
        print_report(fence);
        results.push((
            eager,
            hot_on,
            s1.faults.iter().flatten().sum::<u64>() - s0.faults.iter().flatten().sum::<u64>(),
            s1.eager_pages - s0.eager_pages,
        ));
        out.push(format!(
            "{name}: cpu {:.3} ms, flush {:.3} ms per frame, faults {:.1}, copied by faults {:.1}, copy-back {:.1} pages",
            cpu_ns as f64 / frames as f64 / 1e6,
            flush_ns as f64 / frames as f64 / 1e6,
            (s1.faults.iter().flatten().sum::<u64>() - s0.faults.iter().flatten().sum::<u64>()) as f64 / frames as f64,
            (s1.synced - s0.synced) as f64 / frames as f64,
            (s1.eager_pages - s0.eager_pages) as f64 / frames as f64,
        ));
    }
    pages.set_adaptive(false, false);
    pages.quiesce();
    // The next test allocates from the same heap: leave no page of this guest without access (the allocator may write
    // to the block again before the next `attach`).
    pages.release_all();
    assert_eq!(results[0].3, 0, "nothing copies back with the setting off");
    assert!(
        results[1].3 > 0,
        "the completion copies back the pages game threads waited for"
    );
    assert!(
        results[1].2 < results[0].2,
        "fewer faults with the copy-back"
    );
    assert!(
        results[2].2 < results[1].2,
        "fewer faults with writable pages"
    );
    for l in out {
        eprintln!("adaptive test {l}");
    }
    FORCE.with(|f| f.set(None));
}

#[test]
fn the_stage_pool_gives_a_buffer_back_when_its_frame_is_complete() {
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    let mut pool = StagePool::default();
    let a = pool.take(&ctx, 10).unwrap();
    assert!(a.len() >= 10 * PAGE);
    pool.busy.push((5, a.clone()));
    pool.reclaim(4);
    assert_eq!((pool.busy.len(), pool.free.len()), (1, 0));
    pool.reclaim(5);
    assert_eq!((pool.busy.len(), pool.free.len()), (0, 1));
    let b = pool.take(&ctx, 20).unwrap();
    assert!(Arc::ptr_eq(&a, &b), "the same buffer serves a smaller need");
    // A bigger need makes a bigger buffer and drops the small ones.
    pool.free.push(b);
    let c = pool.take(&ctx, 100_000).unwrap();
    assert!(c.len() >= 100_000 * PAGE);
    assert!(pool.free.is_empty());
}

#[test]
fn the_sink_grows_when_more_pages_come_than_it_was_told() {
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    let mut pool = StagePool::default();
    let mut sink = Sink::new(&ctx, &mut pool);
    sink.begin(1);
    let cap = sink.cap;
    for p in 0..(cap as u32 + 3) {
        sink.stage(p, &vec![p as u8; PAGE as usize]);
    }
    sink.stage(1, &vec![0xee; PAGE as usize]);
    assert!(!sink.failed);
    assert_eq!(sink.slots.len(), cap + 3);
    let buf = sink.buf.clone().unwrap();
    let mut b = [0u8; 4];
    buf.read(u64::from(sink.index[&2]) * PAGE, &mut b).unwrap();
    assert_eq!(b, [2; 4]);
    buf.read(u64::from(sink.index[&1]) * PAGE, &mut b).unwrap();
    assert_eq!(
        b, [0xee; 4],
        "a page that is staged again keeps the later bytes"
    );
}

#[test]
fn the_extent_of_a_stream_follows_the_draw() {
    let s = |stride, divisor, size| StreamDetail {
        index: 0,
        address: 0x1000,
        size,
        stride,
        divisor,
    };
    let arrays = DrawArgs::Arrays {
        first: 2,
        count: 10,
        instances: 4,
        base_instance: 0,
    };
    assert_eq!(stream_extent(&s(32, 0, 1 << 30), &arrays), 32 * 12);
    assert_eq!(stream_extent(&s(32, 2, 1 << 30), &arrays), 32 * 2);
    assert_eq!(
        stream_extent(&s(32, 0, 100), &arrays),
        100,
        "never past the buffer"
    );
    let indexed = DrawArgs::Indexed {
        index_type: 1,
        address: 0,
        count: 10,
        instances: 1,
        base_vertex: 0,
        base_instance: 0,
    };
    assert_eq!(stream_extent(&s(32, 0, 1 << 30), &indexed), UNKNOWN_BYTES);
}

#[test]
fn page_runs_merge_and_answer_membership() {
    let mut r = PageRuns::default();
    r.add(&[(5, 8), (1, 3), (3, 4), (7, 10)]);
    assert_eq!(r.0, vec![(1, 4), (5, 10)]);
    assert!(r.contains(1) && r.contains(3) && r.contains(9));
    assert!(!r.contains(0) && !r.contains(4) && !r.contains(10));
}
