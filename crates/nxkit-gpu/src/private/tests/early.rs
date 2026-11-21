//! Tests of step 6: the cuts after the passes that write the pages a thread waited for, and their signals.

use super::*;
use crate::private::early::{plan_cuts, runs_of_pages};

// ----- step 6: pass signals -----

#[test]
fn the_cuts_follow_the_last_pass_that_writes_a_page_and_the_earliest_ones_are_kept() {
    // Passes 0..5; readers are pages 10, 20, 30, 40, 50, 60.
    let runs = |v: &[u32]| runs_of_pages(v);
    let pass_runs = vec![
        vec![(10, 11)],
        vec![(10, 11), (20, 21)],
        vec![(30, 31)],
        vec![(30, 31), (40, 41)],
        vec![(50, 51)],
        vec![(60, 61)],
    ];
    let readers = [10, 20, 30, 40, 50, 60];
    let (cuts, unserved) = plan_cuts(&readers, &pass_runs, &[], 8);
    assert_eq!(unserved, 1, "page 60 is written by the last pass");
    let at: Vec<usize> = cuts.iter().map(|c| c.after_pass).collect();
    assert_eq!(at, vec![1, 3, 4], "page 10 is written again by pass 1");
    assert_eq!(cuts[0].pages, runs(&[10, 20]));
    assert_eq!(cuts[1].pages, runs(&[30, 40]));
    // With two cuts at most the earliest stays and the last takes the rest.
    let (cuts, _) = plan_cuts(&readers, &pass_runs, &[], 2);
    assert_eq!(cuts.len(), 2);
    assert_eq!(
        (cuts[0].after_pass, cuts[0].pages.clone()),
        (1, runs(&[10, 20]))
    );
    assert_eq!(
        (cuts[1].after_pass, cuts[1].pages.clone()),
        (4, runs(&[30, 40, 50]))
    );
    // A page that some pass writes through a pool of raw stores has no cut.
    let (cuts, unserved) = plan_cuts(&readers, &pass_runs, &[(20, 21), (30, 31)], 8);
    assert_eq!(unserved, 3);
    assert_eq!(
        cuts.iter().map(|c| c.after_pass).collect::<Vec<_>>(),
        vec![1, 3, 4]
    );
    assert_eq!(cuts[0].pages, runs(&[10]));
    assert_eq!(cuts[1].pages, runs(&[40]));
    // A page that no pass writes has none either.
    let (cuts, unserved) = plan_cuts(&[99], &pass_runs, &[], 8);
    assert!(cuts.is_empty());
    assert_eq!(unserved, 1);
}

/// A frame of three passes (a dispatch, a buffer fill, a dispatch): the first dispatch writes `a`, the fill `z`, the last
/// dispatch `y`.
fn three_passes(
    a: u64,
    z: u64,
    y: u64,
) -> (
    nxkit_nvn::frame::Frame,
    nxkit_nvn::passes::PassPlan,
    FrameDetail,
) {
    use crate::detail::CopyDetail;
    use nxkit_nvn::frame::{Cmd, Frame, Span};
    let ubo = |address| UboDetail {
        stage: Stage::Compute,
        slot: 0,
        address,
        size: 64,
        snapshot: None,
    };
    let disp = |at| DispatchDetail {
        program: "cs".into(),
        push: None,
        groups: [1, 1, 1],
        indirect: None,
        ubos: Vec::new(),
        sbufs: vec![ubo(at)],
        textures: Vec::new(),
        images: Vec::new(),
    };
    let d = Cmd::Dispatch {
        reads: Span::default(),
        writes: Span::default(),
    };
    let frame = Frame {
        cmds: vec![d, Cmd::BufferCopy, d],
        ..Frame::default()
    };
    let mut plan = nxkit_nvn::passes::PassPlan::default();
    nxkit_nvn::analyse::Analyser::new()
        .analyse(
            &frame,
            &nxkit_nvn::resource::ResourceTable::new(),
            &mut plan,
        )
        .unwrap();
    let detail = FrameDetail {
        dispatches: vec![disp(a), disp(y)],
        copies: vec![CopyDetail {
            kind: CopyKind::BufferFill,
            dst_addr: z,
            bytes: 32,
            ..Default::default()
        }],
        ..Default::default()
    };
    (frame, plan, detail)
}

/// Reads one byte of `page` on a thread of its own.
fn read_on_thread(g: &Guest, page: u64) -> std::thread::JoinHandle<u8> {
    let mem = g.mem.clone();
    std::thread::spawn(move || {
        let mut b = [0u8; 1];
        mem.read(page * PAGE, &mut b).unwrap();
        b[0]
    })
}

#[test]
fn the_planner_cuts_a_real_frame_after_the_passes_that_write_the_pages_a_thread_waited_for() {
    let _t = locked(&TURN);
    let Some(g) = guest(64 << 20) else { return };
    let (page_a, page_z, page_y) = (80u64, 81u64, 82u64);
    let (frame, plan, detail) =
        three_passes(VA + page_a * PAGE, VA + page_z * PAGE, VA + page_y * PAGE);
    assert_eq!(plan.passes.len(), 3, "dispatch, fill, dispatch");
    let _ = Pages::global().readers(0);
    // Frame 1: three threads wait for the writers of the three pages.
    let _o = at_flush(&detail, 1, &g.map, 0, false, &|| {}).expect("overlay");
    let readers: Vec<_> = [page_a, page_z, page_y]
        .iter()
        .map(|&p| read_on_thread(&g, p))
        .collect();
    std::thread::sleep(std::time::Duration::from_millis(60));
    assert!(readers.iter().all(|r| !r.is_finished()), "the writers run");
    encoded(1, None);
    readers.into_iter().for_each(|r| {
        r.join().unwrap();
    });
    // Frame 2 has the same passes: a cut after the dispatch and one after the fill; the last dispatch's page has none.
    let o2 = at_flush(&detail, 2, &g.map, 0, false, &|| {}).expect("overlay");
    assert_eq!(o2.frame(), 2);
    let pg = |p: u64| {
        let h = g.mem.host_base() + p * PAGE;
        Pages::global().page_range(h, 1).unwrap()
    };
    let plan2 = early_cuts(&frame, &plan, &detail, 2, &g.map, &[]);
    assert_eq!(plan2.readers, 3);
    assert_eq!(plan2.blocked, 1, "the last pass writes page y");
    assert_eq!(plan2.cuts.len(), 2);
    assert_eq!(
        (plan2.cuts[0].after_pass, plan2.cuts[0].pages.clone()),
        (0, vec![pg(page_a)])
    );
    assert_eq!(
        (plan2.cuts[1].after_pass, plan2.cuts[1].pages.clone()),
        (1, vec![pg(page_z)])
    );
    // The same frame with a pool of raw stores over page a: no cut serves it.
    let learned = [(VA + page_a * PAGE, VA + (page_a + 1) * PAGE)];
    let plan3 = early_cuts(&frame, &plan, &detail, 2, &g.map, &learned);
    assert_eq!(plan3.cuts.len(), 1);
    assert_eq!(plan3.cuts[0].after_pass, 1);
    // The pages of a texture that is copied back are left out.
    let mut cuts = plan2.cuts.clone();
    early_exclude(
        &mut cuts,
        &[(VA + page_z * PAGE, VA + (page_z + 1) * PAGE)],
        &g.map,
    );
    assert_eq!(cuts.len(), 1);
    assert_eq!(cuts[0].after_pass, 0);
    encoded(2, None);
    FORCE.with(|f| f.set(None));
}

const WRITE_MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;
kernel void nxkit_test_write(device uint *p [[buffer(0)]]) { p[0] = 0x600df00du; }
kernel void nxkit_test_spin(device uint *p [[buffer(0)]], device const uint *n [[buffer(1)]], uint t [[thread_position_in_grid]]) {
    uint x = t + 1u;
    for (uint i = 0u; i < n[0]; i++) { x = x * 1664525u + 1013904223u; }
    p[t] = x;
}
"#;

/// One compute pass of `cb` that runs `kernel` over one thread with the buffers `bufs` (index, GPU address).
fn kernel_pass(
    pipelines: &Pipelines,
    table: &ArgumentTable,
    cb: &mut CommandBuffer,
    kernel: &str,
    bufs: &[(u32, u64)],
) {
    let pipeline = pipelines
        .host_compute("nxkit-private-test", WRITE_MSL, kernel)
        .unwrap();
    let mut cp = cb.begin_compute_pass(table).unwrap();
    cp.encode(&ComputeCommand::SetPipeline(pipeline)).unwrap();
    for &(index, gpu_addr) in bufs {
        cp.encode(&ComputeCommand::SetBuffer { index, gpu_addr })
            .unwrap();
    }
    cp.encode(&ComputeCommand::DispatchThreads {
        grid: [1, 1, 1],
        threads: [1, 1, 1],
    })
    .unwrap();
    cp.end();
}

#[test]
fn a_reader_of_a_page_written_by_pass_0_goes_on_at_its_signal_while_a_long_pass_1_runs_and_before_the_rest_is_encoded()
 {
    let _t = locked(&TURN);
    let Some(g) = guest(64 << 20) else { return };
    let Ok(pipelines) = Pipelines::new(&g.ctx, &nxkit_metal::ArchiveConfig::default()) else {
        return;
    };
    let (page_a, page_y) = (90u64, 91u64);
    let (frame, plan, detail) =
        three_passes(VA + page_a * PAGE, VA + 95 * PAGE, VA + page_y * PAGE);
    let _ = Pages::global().readers(0);
    let table = ArgumentTable::new(&g.ctx, 31, 0).unwrap();
    let mut ring = nxkit_metal::BatchRing::new(&g.ctx, 3).unwrap();
    // Calibration of the long pass: one thread spins for about `n` rounds.
    let spin = |ring: &mut nxkit_metal::BatchRing, n: u32| -> std::time::Duration {
        let mut batch = ring.begin(1).unwrap();
        let scratch = batch.scratch();
        let out = scratch.push(&[0u8; 64]).unwrap();
        let count = scratch.push(&n.to_le_bytes()).unwrap();
        kernel_pass(
            &pipelines,
            &table,
            &mut batch.buffers()[0],
            "nxkit_test_spin",
            &[(0, out), (1, count)],
        );
        g.ctx.commit_residency();
        let t0 = Instant::now();
        batch.submit().unwrap().wait().unwrap();
        t0.elapsed()
    };
    let _ = spin(&mut ring, 1000);
    let probe = spin(&mut ring, 20_000_000);
    let rounds = (20_000_000f64 * 0.5 / probe.as_secs_f64().max(0.001)).clamp(1e6, 4e9) as u32;
    // Frame 1 teaches the planner: threads wait for the writers of both pages.
    let _o = at_flush(&detail, 1, &g.map, 0, false, &|| {}).expect("overlay");
    let readers: Vec<_> = [page_a, page_y]
        .iter()
        .map(|&p| read_on_thread(&g, p))
        .collect();
    std::thread::sleep(std::time::Duration::from_millis(40));
    encoded(1, None);
    readers.into_iter().for_each(|r| {
        r.join().unwrap();
    });
    // Frame 2: flush, plan, then encode in the order of `encode_frame_inner`.
    let o2 = at_flush(&detail, 2, &g.map, 0, false, &|| {}).expect("overlay");
    let pools = pools_of(&g, o2.clone());
    let loc = pools.resolve(VA + page_a * PAGE, 64).unwrap();
    let mut ep = early_cuts(&frame, &plan, &detail, 2, &g.map, &[]);
    assert_eq!(ep.cuts.len(), 1, "a cut after pass 0: {:?}", ep.cuts);
    assert_eq!(ep.cuts[0].after_pass, 0);
    // Pass 0 is in the command buffer 1 (the command buffer 0 is the first one), the other passes in the command buffer 2.
    let reader = read_on_thread(&g, page_a);
    let other = read_on_thread(&g, page_y);
    let s0 = Pages::global().stats();
    let done_at = std::sync::Arc::new(Mutex::new(None::<Instant>));
    let mut batch = ring.begin(3).unwrap();
    assert!(batch.on_prefix_complete(2, early_release(2, ep.cuts.remove(0).pages)));
    let ahead = batch.commit_ahead().expect("a prefix");
    assert_eq!(ahead.count(), 2);
    let scratch = batch.scratch();
    let out = scratch.push(&[0u8; 64]).unwrap();
    let count = scratch.push(&rounds.to_le_bytes()).unwrap();
    {
        let bufs = batch.buffers();
        ahead.finish(0, &mut bufs[0]);
        // Pass 0 writes the page in the mirror.
        kernel_pass(
            &pipelines,
            &table,
            &mut bufs[1],
            "nxkit_test_write",
            &[(0, loc.real)],
        );
        assert!(
            ahead.finish(1, &mut bufs[1]),
            "the second command buffer completes the group"
        );
    }
    // The group is on the GPU. The rest of the frame is not encoded yet: the reader must not wait for that.
    let t_group = Instant::now();
    let first_byte = reader.join().unwrap();
    let waited = t_group.elapsed();
    assert_eq!(
        first_byte, 0x0d,
        "the bytes of pass 0 (low byte of 0x600df00d)"
    );
    assert!(
        waited < std::time::Duration::from_millis(1500),
        "the reader went on before the rest was encoded: {waited:?}"
    );
    assert!(
        !other.is_finished(),
        "the page of the later pass still waits for the frame"
    );
    // Now the long pass is encoded and the rest is committed.
    {
        let bufs = batch.buffers();
        kernel_pass(
            &pipelines,
            &table,
            &mut bufs[2],
            "nxkit_test_spin",
            &[(0, out), (1, count)],
        );
    }
    g.ctx.commit_residency();
    let submitted = batch.submit().unwrap();
    let t_done = done_at.clone();
    submitted.on_complete(move || *locked(&t_done) = Some(Instant::now()));
    encoded(2, Some(&submitted));
    let t_reader_done = t_group + waited;
    assert!(!other.is_finished() || locked(&done_at).is_some());
    let stats = submitted.wait().unwrap();
    let _ = other.join().unwrap();
    let finished = locked(&done_at).expect("the batch completed");
    assert!(
        t_reader_done + std::time::Duration::from_millis(100) < finished,
        "the reader was released at least 100 ms before the batch completed (long pass {:?})",
        std::time::Duration::from_nanos(stats.gpu_ns)
    );
    eprintln!(
        "pass signal test: reader released {waited:?} after the group was committed, {:?} before the batch completed (long pass {:?})",
        finished.duration_since(t_reader_done),
        std::time::Duration::from_nanos(stats.gpu_ns)
    );
    let s1 = Pages::global().stats();
    assert_eq!(
        s1.wait_ends[0] - s0.wait_ends[0],
        1,
        "one wait ended at the signal of a pass"
    );
    assert!(s1.early_pages > s0.early_pages);
    FORCE.with(|f| f.set(None));
}

#[test]
fn a_batch_with_a_prefix_is_committed_in_two_groups_and_the_prefix_reports_before_the_rest_is_done()
{
    let _t = locked(&TURN);
    let Some(g) = guest(64 << 20) else { return };
    let Ok(pipelines) = Pipelines::new(&g.ctx, &nxkit_metal::ArchiveConfig::default()) else {
        return;
    };
    let table = ArgumentTable::new(&g.ctx, 31, 0).unwrap();
    let mut ring = nxkit_metal::BatchRing::new(&g.ctx, 3).unwrap();
    for with_prefix in [false, true] {
        let mut batch = ring.begin(3).unwrap();
        let scratch = batch.scratch();
        let out = scratch.push(&[0u8; 64]).unwrap();
        let count = scratch.push(&20_000_000u32.to_le_bytes()).unwrap();
        let at = std::sync::Arc::new(Mutex::new(None::<Instant>));
        if with_prefix {
            let a = at.clone();
            assert!(batch.on_prefix_complete(2, move || *locked(&a) = Some(Instant::now())));
            assert!(
                !batch.on_prefix_complete(3, || {}),
                "the whole batch is no prefix"
            );
            assert!(!batch.on_prefix_complete(0, || {}));
        }
        {
            let bufs = batch.buffers();
            kernel_pass(
                &pipelines,
                &table,
                &mut bufs[0],
                "nxkit_test_write",
                &[(0, out)],
            );
            kernel_pass(
                &pipelines,
                &table,
                &mut bufs[1],
                "nxkit_test_write",
                &[(0, out)],
            );
            kernel_pass(
                &pipelines,
                &table,
                &mut bufs[2],
                "nxkit_test_spin",
                &[(0, out), (1, count)],
            );
        }
        g.ctx.commit_residency();
        let submitted = batch.submit().unwrap();
        let end = std::sync::Arc::new(Mutex::new(None::<Instant>));
        let e = end.clone();
        submitted.on_complete(move || *locked(&e) = Some(Instant::now()));
        let stats = submitted.wait().unwrap();
        assert!(stats.gpu_ns > 0);
        let end = locked(&end).expect("the batch completed");
        if with_prefix {
            let prefix = locked(&at).expect("the prefix reported");
            assert!(
                prefix <= end,
                "the prefix reports before the batch completes"
            );
        } else {
            assert!(locked(&at).is_none());
        }
    }
    FORCE.with(|f| f.set(None));
}
