//! Tests of the mirror: members, staging, the copy-in pass, CPU stores outside the written bytes and poison.

use super::*;

#[test]
fn a_written_binding_and_a_uniform_that_touches_it_resolve_to_the_mirror() {
    let _t = locked(&TURN);
    let Some(g) = guest(64 << 20) else { return };
    let page = PAGE;
    // A storage binding of 1 KiB in page 10; its uniform is 0x8000 further (page 12), not a member.
    let addr = VA + 10 * page + 0x100;
    g.mem.write(10 * page + 0x100, &[7; 16]).unwrap();
    let detail = dispatch(addr, 0x400);
    let o = at_flush(&detail, 1, &g.map, 0, false, &|| {}).expect("overlay");
    let pools = pools_of(&g, o.clone());
    let loc = pools.resolve(addr, 0x400).unwrap();
    assert_ne!(
        loc.run,
        crate::pools::NO_RUN,
        "the written range lies in the mirror"
    );
    let loc_ubo = pools.resolve_start(addr + 0x8000).unwrap();
    assert_eq!(
        loc_ubo.run,
        crate::pools::NO_RUN,
        "an untouched uniform stays in guest memory"
    );
    // The mirror run holds the guest bytes as they were at the flush.
    let run = &o.runs()[loc.run as usize];
    let mut b = [0u8; 16];
    run.mem.read(loc.offset, &mut b).unwrap();
    assert_eq!(b, [7; 16]);
    assert_eq!(
        Pages::global().state_of(g.mem.host_base() + 10 * page),
        "gpu(1)"
    );
    // The GPU writes the mirror and the frame completes; the CPU's read copies the page.
    run.mem.write(loc.offset, &[9; 16]).unwrap();
    encoded(1, None);
    let mut seen = [0u8; 16];
    g.mem.read(10 * page + 0x100, &mut seen).unwrap();
    assert_eq!(
        seen, [9; 16],
        "the read of the stale page found the GPU's bytes"
    );
    assert_eq!(
        Pages::global().state_of(g.mem.host_base() + 10 * page),
        "clean"
    );
    FORCE.with(|f| f.set(None));
}

#[test]
fn a_range_that_a_binding_reaches_into_a_member_makes_its_pages_members_too() {
    let _t = locked(&TURN);
    let Some(g) = guest(64 << 20) else { return };
    let page = PAGE;
    // The storage binding is in page 20; a uniform that starts in page 19 and ends in page 20 touches it.
    let mut detail = dispatch(VA + 20 * page, 64);
    detail.dispatches[0].ubos[0].address = VA + 20 * page - 0x100;
    detail.dispatches[0].ubos[0].size = 0x200;
    let o = at_flush(&detail, 1, &g.map, 0, false, &|| {}).expect("overlay");
    assert!(
        o.member(VA + 19 * page + 0x3f00),
        "the part before the member is a member"
    );
    assert!(o.all_members(VA + 20 * page - 0x100, 0x200));
    let pools = pools_of(&g, o);
    let loc = pools.resolve(VA + 20 * page - 0x100, 0x200).unwrap();
    assert_ne!(loc.run, crate::pools::NO_RUN);
    assert_eq!(crate::overlay::partial(), crate::overlay::partial());
    encoded(1, None);
    FORCE.with(|f| f.set(None));
}

#[test]
fn a_page_the_cpu_wrote_is_staged_and_the_copy_in_pass_moves_it_into_the_mirror() {
    let _t = locked(&TURN);
    let Some(g) = guest(64 << 20) else { return };
    let Ok(pipelines) = Pipelines::new(&g.ctx, &nxkit_metal::ArchiveConfig::default()) else {
        return;
    };
    let page = PAGE;
    let addr = VA + 30 * page;
    let detail = dispatch(addr, 0x100);
    // Frame 1 makes page 30 a member and the GPU writes it.
    let o1 = at_flush(&detail, 1, &g.map, 0, false, &|| {}).expect("overlay");
    let pools = pools_of(&g, o1.clone());
    let loc = pools.resolve(addr, 0x100).unwrap();
    let run = &o1.runs()[loc.run as usize];
    run.mem.write(loc.offset, &[5; 8]).unwrap();
    encoded(1, None);
    // The CPU reads (the page is copied), then writes a byte next to the GPU's: the page is dirty.
    let mut b = [0u8; 8];
    g.mem.read(30 * page, &mut b).unwrap();
    assert_eq!(b, [5; 8]);
    g.mem.write(30 * page + 8, &[6; 8]).unwrap();
    assert_eq!(
        Pages::global().state_of(g.mem.host_base() + 30 * page),
        "dirty"
    );
    // Frame 2 stages the page whole, and the first command buffer of the frame copies it into the mirror.
    let o2 = at_flush(&detail, 2, &g.map, 0, false, &|| {}).expect("overlay");
    let staged = o2.staged().expect("a staged page");
    assert_eq!(staged.slots.len(), 1);
    assert_eq!(
        Pages::global().state_of(g.mem.host_base() + 30 * page),
        "gpu(2, fresh)"
    );
    // The mirror still holds the state before the CPU's write.
    let pools2 = pools_of(&g, o2.clone());
    let loc2 = pools2.resolve(addr, 0x100).unwrap();
    let run2 = &o2.runs()[loc2.run as usize];
    let mut m = [0u8; 16];
    run2.mem.read(loc2.offset, &mut m).unwrap();
    assert_eq!(&m[8..], &[0; 8]);
    // The frame's view reads the staged bytes for the CPU's page.
    let mut v = [0u8; 16];
    assert!(o2.read(addr, &mut v));
    assert_eq!(&v[..8], &[5; 8]);
    assert_eq!(&v[8..], &[6; 8]);
    let (jobs, lost) = copy_in_jobs(&o2);
    assert_eq!((jobs.len(), lost), (1, 0));
    let scratch = Scratch::new(&g.ctx, 1 << 16).unwrap();
    let table = ArgumentTable::new(&g.ctx, 31, 0).unwrap();
    let mut ring = nxkit_metal::FrameRing::new(&g.ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    assert_eq!(
        encode_copy_in(&pipelines, frame.commands(), &table, &scratch, &jobs).unwrap(),
        1
    );
    g.ctx.commit_residency();
    frame.submit().unwrap().wait().unwrap();
    run2.mem.read(loc2.offset, &mut m).unwrap();
    assert_eq!(&m[..8], &[5; 8], "the GPU's bytes are kept");
    assert_eq!(&m[8..], &[6; 8], "and the CPU's landed");
    encoded(2, None);
    FORCE.with(|f| f.set(None));
}

#[test]
fn the_gpu_copies_late_pages_as_the_frame_starts_and_ends_and_the_completion_hands_the_reader_the_frames_bytes()
 {
    let _t = locked(&TURN);
    let Some(g) = guest(64 << 20) else { return };
    let Ok(pipelines) = Pipelines::new(&g.ctx, &nxkit_metal::ArchiveConfig::default()) else {
        return;
    };
    if !Pages::global().emulating() {
        eprintln!("skipped: no second mapping of guest memory here");
        return;
    }
    let page = PAGE;
    let addr = VA + 70 * page;
    let detail = dispatch(addr, 0x100);
    let peek = || {
        let mut b = [0u8; 8];
        g.mem.read(70 * page, &mut b).unwrap();
        b
    };
    // Frame 1: the GPU writes 5s; the CPU reads them after the completion, which makes the page a CPU-read result.
    let o1 = at_flush(&detail, 1, &g.map, 0, false, &|| {}).expect("overlay");
    let loc = pools_of(&g, o1.clone()).resolve(addr, 0x100).unwrap();
    o1.runs()[loc.run as usize]
        .mem
        .write(loc.offset, &[5; 8])
        .unwrap();
    encoded(1, None);
    assert_eq!(peek(), [5; 8]);
    Pages::global().set_late(true);
    // Frame 2: the copies at its start and end, around the GPU's write of 7s.
    let o2 = at_flush(&detail, 2, &g.map, 0, false, &|| {}).expect("overlay");
    let (start, end, start_at, end_at) = late_copy_jobs(&o2).expect("the page is read late");
    assert_eq!((start.len(), end.len()), (1, 1));
    let scratch = Scratch::new(&g.ctx, 1 << 16).unwrap();
    let table = ArgumentTable::new(&g.ctx, 31, 0).unwrap();
    let mut ring = nxkit_metal::FrameRing::new(&g.ctx).unwrap();
    let mut f = ring.begin().unwrap();
    assert_eq!(
        encode_copy_in(&pipelines, f.commands(), &table, &scratch, &start).unwrap(),
        1
    );
    g.ctx.commit_residency();
    f.submit().unwrap().wait().unwrap();
    let loc2 = pools_of(&g, o2.clone()).resolve(addr, 0x100).unwrap();
    let mem2 = &o2.runs()[loc2.run as usize].mem;
    mem2.write(loc2.offset, &[7; 8]).unwrap();
    let mut f = ring.begin().unwrap();
    assert_eq!(
        encode_copy_in(&pipelines, f.commands(), &table, &scratch, &end).unwrap(),
        1
    );
    f.submit().unwrap().wait().unwrap();
    late_copies_encoded(2, start_at, end_at);
    let l = o2.late().expect("copies");
    let mut b = [0u8; 8];
    l.buf.read(0, &mut b).unwrap();
    assert_eq!(b, [5; 8], "the copy at the start");
    l.buf.read(page, &mut b).unwrap();
    assert_eq!(b, [7; 8], "the copy at the end");
    // The next frame changes the mirror before the completion of frame 2 is reported.
    mem2.write(loc2.offset, &[8; 8]).unwrap();
    encoded(2, None);
    // The read finds the page of a complete frame whose completion copied its bytes: no copy from the mirror again.
    assert_eq!(
        peek(),
        [7; 8],
        "frame 2's bytes, from the copy at its end, not the newer bytes of the mirror"
    );
    Pages::global().set_late(false);
    FORCE.with(|f| f.set(None));
}

#[test]
fn a_cpu_store_outside_the_bytes_a_frame_writes_needs_no_wait_and_reaches_the_next_frame_beside_the_gpus_bytes()
 {
    let _t = locked(&TURN);
    let Some(g) = guest(64 << 20) else { return };
    let Ok(pipelines) = Pipelines::new(&g.ctx, &nxkit_metal::ArchiveConfig::default()) else {
        return;
    };
    if !Pages::global().emulating() {
        eprintln!("skipped: no second mapping of guest memory here");
        return;
    }
    let page = PAGE;
    let addr = VA + 50 * page;
    let detail = dispatch(addr, 0x100);
    // Frame 1 writes the first 0x100 bytes of page 50; the GPU writes eight of them in the mirror.
    let o1 = at_flush(&detail, 1, &g.map, 0, false, &|| {}).expect("overlay");
    let pools = pools_of(&g, o1.clone());
    let loc = pools.resolve(addr, 0x100).unwrap();
    o1.runs()[loc.run as usize]
        .mem
        .write(loc.offset, &[5; 8])
        .unwrap();
    // The frame is in flight. The CPU stores and loads elsewhere in the page and does not wait.
    let s0 = Pages::global().stats();
    let t0 = Instant::now();
    g.mem.write(50 * page + 0x2000, &[9; 8]).unwrap();
    let mut b = [0u8; 8];
    g.mem.read(50 * page + 0x3000, &mut b).unwrap();
    g.mem.read(50 * page + 0x2000, &mut b).unwrap();
    assert_eq!(b, [9; 8], "a load sees the store");
    assert!(
        t0.elapsed() < std::time::Duration::from_millis(500),
        "no wait for the frame"
    );
    let s1 = Pages::global().stats();
    assert_eq!(s1.writer_waits, s0.writer_waits);
    assert!(
        s1.emulated.iter().sum::<u64>() > s0.emulated.iter().sum::<u64>(),
        "the handler did them"
    );
    // Frame 2 stages the page without the blocks that frame 1 writes.
    let o2 = at_flush(&detail, 2, &g.map, 0, false, &|| {}).expect("overlay");
    let staged = o2.staged().expect("a staged page");
    assert_eq!(staged.slots.len(), 1);
    assert_eq!(staged.skip_of(50), Some([0b1111, 0, 0, 0]));
    // The frame's view: the CPU's bytes from the staging buffer, the GPU's from the mirror.
    let mut v = [0u8; 8];
    assert!(o2.read(addr + 0x2000, &mut v));
    assert_eq!(v, [9; 8]);
    assert!(o2.read(addr, &mut v));
    assert_eq!(v, [5; 8]);
    let (jobs, lost) = copy_in_jobs(&o2);
    assert_eq!((jobs.len(), lost), (1, 0));
    assert_eq!(jobs[0].2, [0b1111, 0, 0, 0]);
    let scratch = Scratch::new(&g.ctx, 1 << 16).unwrap();
    let table = ArgumentTable::new(&g.ctx, 31, 0).unwrap();
    let mut ring = nxkit_metal::FrameRing::new(&g.ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    assert_eq!(
        encode_copy_in(&pipelines, frame.commands(), &table, &scratch, &jobs).unwrap(),
        1
    );
    g.ctx.commit_residency();
    frame.submit().unwrap().wait().unwrap();
    let pools2 = pools_of(&g, o2.clone());
    let loc2 = pools2.resolve(addr, 0x100).unwrap();
    let run2 = &o2.runs()[loc2.run as usize];
    let mut m = [0u8; 8];
    run2.mem.read(loc2.offset, &mut m).unwrap();
    assert_eq!(
        m, [5; 8],
        "the GPU's bytes were not overwritten by the stale guest bytes"
    );
    let loc3 = pools2.resolve(addr + 0x2000, 8).unwrap();
    o2.runs()[loc3.run as usize]
        .mem
        .read(loc3.offset, &mut m)
        .unwrap();
    assert_eq!(m, [9; 8], "the CPU's store landed in the mirror");
    encoded(1, None);
    // After frame 1 the CPU reads its bytes: the GPU's arrive, the store stays.
    encoded(2, None);
    g.mem.read(50 * page, &mut b).unwrap();
    assert_eq!(b, [5; 8]);
    g.mem.read(50 * page + 0x2000, &mut b).unwrap();
    assert_eq!(b, [9; 8]);
    FORCE.with(|f| f.set(None));
}

#[test]
fn the_poison_knob_fills_the_stale_guest_bytes_of_a_page_a_host_thread_reads_in_flight() {
    let _t = locked(&TURN);
    let Some(g) = guest(64 << 20) else { return };
    FORCE_POISON.with(|f| f.set(true));
    let page = PAGE;
    let addr = VA + 40 * page;
    // The page is a member; the CPU writes it, and the frame that writes it starts: it is fresh, guest memory holds
    // its bytes, and the poison covers them.
    let o = at_flush(&dispatch(addr, 0x100), 1, &g.map, 0, false, &|| {}).expect("overlay");
    encoded(1, None);
    g.mem.write(40 * page + 4, &[3; 4]).unwrap();
    let o2 = at_flush(&dispatch(addr, 0x100), 2, &g.map, 0, false, &|| {}).expect("overlay");
    assert!(o2.staged().is_some());
    drop(o);
    let host = g.mem.host_base() + 40 * page;
    let seen = std::thread::spawn(move || {
        let _g = nxkit_metal::mark_internal_thread();
        // SAFETY of the read is the tracker's: the page is open for reading after the fault.
        let mut b = [0u8; 8];
        // The page is in flight and fresh: the internal thread gets guest memory as it is.
        nxkit_abi::guest::set_range(host - 40 * page, 64 << 20);
        nxkit_abi::guest::with_slice(host, 8, |s| b.copy_from_slice(s)).unwrap();
        b
    })
    .join()
    .unwrap();
    nxkit_abi::guest::set_range(0, 0);
    assert_eq!(
        seen,
        [0x00, 0x00, 0xc0, 0x7f, 0x00, 0x00, 0xc0, 0x7f],
        "the pattern, not the CPU's bytes"
    );
    encoded(2, None);
    FORCE_POISON.with(|f| f.set(false));
    FORCE.with(|f| f.set(None));
}

#[test]
fn a_may_write_page_the_gpu_did_not_write_equals_the_mirror_after_the_frame() {
    let _t = locked(&TURN);
    let Some(g) = guest(64 << 20) else { return };
    let page = PAGE;
    // The program is unknown, so the binding counts as written; the GPU writes nothing.
    let addr = VA + 60 * page;
    g.mem.write(60 * page + 8, &[0x21; 8]).unwrap();
    let o = at_flush(&dispatch(addr, 0x100), 1, &g.map, 0, false, &|| {}).expect("overlay");
    encoded(1, None);
    let mut b = [0u8; 8];
    g.mem.read(60 * page + 8, &mut b).unwrap();
    assert_eq!(b, [0x21; 8]);
    let pools = pools_of(&g, o.clone());
    let loc = pools.resolve(addr, 0x100).unwrap();
    let mut m = [0u8; 16];
    o.runs()[loc.run as usize]
        .mem
        .read(loc.offset + 8 - 8, &mut m)
        .unwrap();
    assert_eq!(&m[8..], &[0x21; 8]);
    FORCE.with(|f| f.set(None));
}

#[test]
fn with_poison_threads_that_read_after_the_flush_wait_and_get_the_real_bytes() {
    let _t = locked(&TURN);
    let Some(g) = guest(64 << 20) else { return };
    FORCE_POISON.with(|f| f.set(true));
    let page = PAGE;
    // 200 member pages that the frame writes; the CPU wrote each before.
    let first = 100u64;
    for p in 0..200u64 {
        g.mem.write((first + p) * page, &[p as u8 + 1; 8]).unwrap();
    }
    let detail = dispatch(VA + first * page, (200 * page) as u32);
    let _o = at_flush(&detail, 1, &g.map, 0, false, &|| {}).expect("overlay");
    let mem = g.mem.clone();
    let readers: Vec<_> = (0..4)
        .map(|t| {
            let mem = mem.clone();
            std::thread::spawn(move || {
                let mut bad = 0;
                for p in (t..200u64).step_by(4) {
                    let mut b = [0u8; 8];
                    mem.read((first + p) * page, &mut b).unwrap();
                    if b != [p as u8 + 1; 8] {
                        bad += 1;
                    }
                }
                bad
            })
        })
        .collect();
    std::thread::sleep(std::time::Duration::from_millis(50));
    encoded(1, None);
    let bad: u32 = readers.into_iter().map(|h| h.join().unwrap()).sum();
    assert_eq!(bad, 0, "no reader saw the pattern");
    FORCE_POISON.with(|f| f.set(false));
    FORCE.with(|f| f.set(None));
}
