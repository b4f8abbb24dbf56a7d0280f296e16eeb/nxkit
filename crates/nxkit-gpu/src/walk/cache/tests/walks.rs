//! The tests of the walks with templates against the walks without.

use super::*;

#[test]
fn a_nested_recording_plays_from_its_template() {
    for_both(|p| {
        let child = p.put(&model(UBO_AT, 2));
        let top = p.put(&[
            program(PROG),
            viewport(100.0),
            Record::Call(child),
            Record::Call(child),
            draw(),
            viewport(200.0),
            Record::Call(child),
        ]);
        let (d, c) = p.frame(&[top]);
        assert_eq!(d.draws.len(), 7);
        assert_eq!(
            (c.walk_hits, c.walk_misses, c.draws_from_template),
            (1, 2, 2)
        );
        let (d, c) = p.frame(&[top]);
        assert_eq!(d.draws.len(), 7);
        assert_eq!(
            (c.walk_hits, c.walk_misses, c.draws_from_template),
            (3, 0, 6)
        );
        assert_eq!(p.cached.w.cache.templates(child), 2);
        assert!(
            c.records < 12,
            "a hit does not read the records of the callee: {}",
            c.records
        );
        assert_eq!(d.draws[0].dynamic.viewport[2], 100.0);
        assert_eq!(d.draws[6].dynamic.viewport[2], 200.0);
    });
}

#[test]
fn a_hit_shares_the_draws_with_its_template() {
    let mut p = Pair::new(false);
    let child = p.put(&model(UBO_AT, 3));
    let top = p.put(&[program(PROG), Record::Call(child)]);
    let (a, _) = p.frame(&[top]);
    let (b, _) = p.frame(&[top]);
    assert!(a.draws.iter().zip(&b.draws).all(|(x, y)| Arc::ptr_eq(x, y)));
}

#[test]
fn inherited_state_picks_the_template() {
    const OTHER: u64 = GPU + 0xa000;
    for_both(|p| {
        for rig in [&mut p.cached, &mut p.plain] {
            rig.w.program_set(OTHER, &keys(5, 6));
        }
        let child = p.put(&[draw(), clear(), dispatch()]);
        let first = vec![
            program(PROG),
            viewport(10.0),
            Record::BindTexture { slot: 3, handle: 7 },
            Record::Call(child),
        ];
        let calls = [
            first.clone(),
            first.clone(),
            vec![viewport(20.0), Record::Call(child)],
            vec![
                Record::BindTexture { slot: 3, handle: 8 },
                Record::Call(child),
            ],
            vec![program(OTHER), Record::Call(child)],
            first,
        ];
        let tops: Vec<u64> = calls.iter().map(|r| p.put(r)).collect();
        let (d, c) = p.frame(&tops);
        assert_eq!(d.draws.len(), 6);
        assert_eq!((c.walk_hits, c.walk_misses), (2, 4));
        assert_ne!(d.draws[0], d.draws[2]);
        assert_eq!(&*d.draws[4].program.vertex, "0505050505050505");
        let (_, c) = p.frame(&tops);
        assert_eq!((c.walk_hits, c.walk_misses), (6, 0));
        assert_eq!(p.cached.w.cache.templates(child), 4);
    });
}

#[test]
fn state_that_the_callee_sets_stays_set_for_the_caller() {
    for_both(|p| {
        let child = p.put(&[
            program(PROG),
            viewport(33.0),
            ubo(2, UBO_AT, 32),
            blend(2, 5),
            stencil_ref(2, 12),
            ext(ExtCode::Setting, Setting::CopyRowStride as u16, &[256, 0]),
            Record::BindTexture {
                slot: 40,
                handle: 11,
            },
        ]);
        let top = p.put(&[Record::Call(child), draw(), dispatch()]);
        for _ in 0..3 {
            let (d, _) = p.frame(&[top]);
            assert_eq!(d.draws[0].dynamic.viewport[2], 33.0);
            assert_eq!(d.draws[0].dynamic.stencil_ref[1], 12);
            assert_eq!(d.draws[0].blend[2].src_rgb, 5);
        }
    });
}

#[test]
fn mutated_deferred_handles_patch_the_textures_of_the_draws() {
    for_both(|p| {
        let set = |p: &mut Pair, a: u64, b: u64, c: u64| {
            let bytes: Vec<u8> = [a, b, c].iter().flat_map(|v| v.to_le_bytes()).collect();
            p.write(SRC, &bytes);
        };
        set(p, 5, 6, 7);
        let mut m = vec![deferred(0, 2, SRC), deferred(64, 1, SRC + 16)];
        m.extend(model(UBO_AT, 2));
        m.push(dispatch());
        let child = p.put(&m);
        let mid = p.put(&[program(PROG), Record::Call(child), draw()]);
        let top = p.put(&[Record::Call(mid), Record::Call(mid)]);
        let (d, c) = p.frame(&[top]);
        assert_eq!((c.walk_hits, c.walk_misses), (1, 2));
        assert_eq!(
            d.draws[0]
                .textures
                .iter()
                .map(|t| t.handle)
                .collect::<Vec<_>>(),
            [5, 6]
        );
        for values in [
            (5, 6, 7),
            (5, 6, 7),
            (9, 6, 7),
            (9, 10, 11),
            (9, 10, 11),
            (0, 0, 0),
            (5, 6, 7),
        ] {
            set(p, values.0, values.1, values.2);
            let (d, c) = p.frame(&[top]);
            assert_eq!(c.walk_hits, 2, "{values:?}");
            assert_eq!(
                d.draws[3]
                    .textures
                    .iter()
                    .map(|t| t.handle)
                    .collect::<Vec<_>>(),
                [values.0, values.1]
            );
            assert_eq!(
                d.draws[2]
                    .textures
                    .iter()
                    .map(|t| t.handle)
                    .collect::<Vec<_>>(),
                [values.0, values.1]
            );
            assert_eq!(d.dispatches[1].textures[0].handle, values.2);
        }
    });
}

#[test]
fn a_rewritten_recording_drops_its_templates_and_those_of_its_callers() {
    for_both(|p| {
        let child = p.put(&model(UBO_AT, 2));
        let mid = p.put(&[program(PROG), Record::Call(child)]);
        let top = p.put(&[Record::Call(mid)]);
        p.frame(&[top]);
        let (_, c) = p.frame(&[top]);
        assert_eq!((c.walk_hits, c.walk_misses), (1, 0));
        p.put_at(child, &model(UBO_AT, 5));
        p.rewritten(child, 0x100);
        let (d, c) = p.frame(&[top]);
        assert_eq!(d.draws.len(), 5);
        assert_eq!(c.invalidations, 2);
        assert_eq!((c.walk_hits, c.walk_misses), (0, 2));
        let (d, c) = p.frame(&[top]);
        assert_eq!((d.draws.len(), c.walk_hits), (5, 1));
        // A write elsewhere changes nothing.
        p.rewritten(child + 0x800, 0x10);
        assert_eq!(p.frame(&[top]).1.walk_hits, 1);
    });
}

#[test]
fn a_partial_update_with_a_rewritten_tail_gets_a_new_snapshot() {
    for_both(|p| {
        let mut m = vec![ubo(0, UBO_AT, 64), update(UBO_AT, 64, 16, 0xaabbccdd)];
        m.extend([
            vertex_buffer(0, GPU + 0x2000, 256),
            attribute(0, 0x0a, 0),
            stream(0, 16),
            draw(),
        ]);
        let child = p.put(&m);
        let top = p.put(&[program(PROG), Record::Call(child), Record::Call(child)]);
        for round in 0u8..4 {
            // The game rewrites the unchanged tail of the buffer before each submit and says nothing about it.
            p.write(HOST + 0x1000 + 48, &[round + 1; 8]);
            let (d, c) = p.frame(&[top]);
            assert_eq!(c.walk_hits, if round == 0 { 1 } else { 2 });
            for draw in &d.draws {
                let snap = draw.ubos[0].snapshot.as_ref().unwrap();
                assert_eq!(snap.len(), 64);
                assert_eq!(&snap[16..20], &0xaabbccddu32.to_le_bytes());
                assert_eq!(&snap[48..56], &[round + 1; 8]);
            }
        }
    });
}

#[test]
fn an_update_from_another_recording_gives_a_snapshot_to_a_cached_draw() {
    for_both(|p| {
        let reader = p.put(&model(UBO_AT + 0x100, 1));
        let writer = p.put(&[update(UBO_AT + 0x100, 64, 0, 0x01020304)]);
        let top = p.put(&[program(PROG), Record::Call(reader)]);
        let later = p.put(&[program(PROG), Record::Call(writer), Record::Call(reader)]);
        let (d, _) = p.frame(&[top]);
        assert!(d.draws[0].ubos[0].snapshot.is_none());
        let (d, c) = p.frame(&[top]);
        assert_eq!(c.walk_hits, 1);
        assert!(d.draws[0].ubos[0].snapshot.is_none());
        let (d, c) = p.frame(&[later]);
        assert_eq!(c.walk_hits, 1);
        assert_eq!(
            &d.draws[0].ubos[0].snapshot.as_ref().unwrap()[..4],
            &0x01020304u32.to_le_bytes()
        );
        p.frame(&[top, later, top]);
    });
}

#[test]
fn an_update_between_draws_gives_each_draw_the_bytes_it_saw() {
    for_both(|p| {
        let mut m = model(UBO_AT, 1);
        m.extend([
            update(UBO_AT, 64, 0, 1),
            draw(),
            dispatch(),
            update(UBO_AT, 64, 0, 2),
            draw(),
        ]);
        let child = p.put(&m);
        let top = p.put(&[program(PROG), Record::Call(child)]);
        for round in 0..3 {
            let (d, _) = p.frame(&[top]);
            let seen = |i: usize| d.draws[i].ubos[0].snapshot.as_ref().map(|s| s[0]);
            // After the first frame the range is in the history of updates, and the first draw sees the last write.
            let before = if round == 0 { None } else { Some(2) };
            assert_eq!((seen(0), seen(1), seen(2)), (before, Some(1), Some(2)));
        }
    });
}

#[test]
fn program_changes_make_templates_stale() {
    for_both(|p| {
        let child = p.put(&model(UBO_AT, 2));
        let top = p.put(&[program(PROG), Record::Call(child)]);
        p.frame(&[top]);
        assert_eq!(p.frame(&[top]).1.walk_hits, 1);
        for rig in [&mut p.cached, &mut p.plain] {
            rig.w.program_set(PROG, &keys(3, 4));
        }
        let (d, c) = p.frame(&[top]);
        assert_eq!((c.walk_hits, c.walk_misses), (0, 1));
        assert_eq!(&*d.draws[0].program.vertex, "0303030303030303");
        assert_eq!(p.frame(&[top]).1.walk_hits, 1);
        // The same stages again change nothing.
        for rig in [&mut p.cached, &mut p.plain] {
            rig.w.program_set(PROG, &keys(3, 4));
        }
        assert_eq!(p.frame(&[top]).1.walk_hits, 1);
        for rig in [&mut p.cached, &mut p.plain] {
            rig.w.program_released(PROG);
        }
        let (d, c) = p.frame(&[top]);
        assert_eq!(c.walk_hits, 0);
        assert_eq!(&*d.draws[0].program.vertex, "");
    });
}

#[test]
fn a_handle_keeps_at_most_max_per_handle_templates() {
    const N: usize = MAX_PER_HANDLE + 2;
    for_both(|p| {
        let child = p.put(&[draw()]);
        let calls: Vec<Record> = [program(PROG)]
            .into_iter()
            .chain((0..N).flat_map(|i| [viewport(10.0 + i as f32), Record::Call(child)]))
            .collect();
        let top = p.put(&calls);
        let (_, c) = p.frame(&[top]);
        assert_eq!((c.walk_misses, c.evictions), (N as u32, 2));
        assert_eq!(p.cached.w.cache.templates(child), MAX_PER_HANDLE);
        for _ in 0..3 {
            p.frame(&[top]);
            assert_eq!(p.cached.w.cache.templates(child), MAX_PER_HANDLE);
        }
        let few: Vec<Record> = [program(PROG)]
            .into_iter()
            .chain((2..N).flat_map(|i| [viewport(10.0 + i as f32), Record::Call(child)]))
            .collect();
        let top = p.put(&few);
        p.frame(&[top]);
        assert_eq!(p.frame(&[top]).1.walk_hits, MAX_PER_HANDLE as u32);
    });
}

#[test]
fn a_handle_with_more_states_than_templates_turns_cold() {
    let n = MAX_PER_HANDLE * 6;
    for_both(|p| {
        let child = p.put(&[draw()]);
        let calls: Vec<Record> = [program(PROG)]
            .into_iter()
            .chain((0..n).flat_map(|i| [viewport(10.0 + i as f32), Record::Call(child)]))
            .collect();
        let top = p.put(&calls);
        p.frame(&[top]);
        assert_eq!(p.cached.w.cache.templates(child), 0);
        let (d, c) = p.frame(&[top]);
        assert_eq!(
            (c.cold_walks as usize, c.walk_misses as usize, d.draws.len()),
            (n, n, n)
        );
    });
}

#[test]
fn a_chain_of_calls_deeper_than_the_limit_is_cut_the_same_way() {
    for_both(|p| {
        let mut next = p.put(&[draw()]);
        let mut chain = vec![next];
        for _ in 0..9 {
            next = p.put(&[draw(), Record::Call(next)]);
            chain.push(next);
        }
        let (first, last) = (chain[chain.len() - 1], chain[0]);
        let mid = chain[chain.len() - 4];
        let top = p.put(&[
            Record::Call(mid),
            Record::Call(first),
            Record::Call(mid),
            Record::Call(last),
        ]);
        for _ in 0..3 {
            p.frame(&[top]);
        }
        let me = p.put(&[Record::Call(me_self(p))]);
        p.frame(&[me]);
    });
}

/// The handle that the next `put` returns, so that a recording can call itself.
fn me_self(p: &Pair) -> u64 {
    p.cached.at
}

#[test]
fn clears_copies_and_reports_come_from_the_template() {
    for_both(|p| {
        let region = [0, 0, 0, 4, 4, 1];
        let words: Vec<u32> = [0x2000, 0, 0x100, 0, 0].into_iter().chain(region).collect();
        let child = p.put(&[
            clear(),
            ext(ExtCode::ClearDepthStencil, 0, &[1f32.to_bits(), 1, 0, 0xff]),
            ext(ExtCode::Setting, Setting::CopyRowStride as u16, &[512, 0]),
            ext(ExtCode::CopyBufferToTexture, 0, &words),
            ext(ExtCode::ReportCounter, 0, &[0, 0x40, 0]),
            Record::Copy { src: 0, dst: 0 },
            ext(ExtCode::ClearBuffer, 0, &[0x100, 0, 4, 0, 0]),
        ]);
        let top = p.put(&[Record::Call(child), Record::Call(child)]);
        for _ in 0..3 {
            let (d, _) = p.frame(&[top]);
            assert_eq!((d.clears.len(), d.copies.len(), d.reports.len()), (4, 6, 2));
            assert_eq!(d.copies[0].row_stride, 512);
        }
    });
}

#[test]
fn the_walker_and_the_decoder_count_the_same_entries_with_templates() {
    use nxkit_nvn::frame::Cmd;
    use nxkit_nvn::front::FrontEnd;
    use nxkit_nvn::space::LinearSpace;

    let mut f = FrontEnd::new(
        LinearSpace::new(HOST, GPU, SIZE).unwrap(),
        VecMem::new(HOST, SIZE as usize),
    );
    let mut rig = Rig::new();
    let region = [0, 0, 0, 4, 4, 1];
    let words: Vec<u32> = [0x2000, 0, 0x100, 0, 0].into_iter().chain(region).collect();
    let child_recs = vec![
        program(PROG),
        draw(),
        clear(),
        draw(),
        dispatch(),
        ext(ExtCode::ClearBuffer, 0, &[0x100, 0, 4, 0, 0]),
        update(UBO_AT, 16, 0, 3),
        ext(ExtCode::CopyBufferToTexture, 0, &words),
        Record::Copy { src: 0, dst: 0 },
    ];
    let record = |f: &mut FrontEnd<LinearSpace, VecMem>, cb: usize, at: u64, recs: &[Record]| {
        f.add_command_memory(cb, at, 4096).unwrap();
        f.begin_recording(cb).unwrap();
        for r in recs {
            f.write(cb, *r).unwrap();
        }
        f.end_recording(cb).unwrap()
    };
    let child = record(&mut f, 0, CODE, &child_recs);
    rig.put_at(CODE, &child_recs);
    let top_recs = [
        Record::Call(child.0),
        draw(),
        Record::Call(child.0),
        Record::Call(child.0),
    ];
    let top = record(&mut f, 1, CODE + 0x1000, &top_recs);
    rig.put_at(CODE + 0x1000, &top_recs);
    for round in 0..3 {
        f.begin_frame();
        f.submit(top).unwrap();
        let mut n = [0usize; 4];
        for c in &f.frame().cmds {
            match c {
                Cmd::Draw { .. } => n[0] += 1,
                Cmd::ClearColor { .. } | Cmd::ClearDepthStencil { .. } => n[1] += 1,
                Cmd::Dispatch { .. } => n[2] += 1,
                Cmd::Copy { .. } | Cmd::Downsample { .. } | Cmd::BufferCopy => n[3] += 1,
                _ => {}
            }
        }
        let (d, c) = rig.frame(&[top.0]);
        assert_eq!(
            (
                d.draws.len(),
                d.clears.len(),
                d.dispatches.len(),
                d.copies.len()
            ),
            (n[0], n[1], n[2], n[3])
        );
        assert_eq!(n, [7, 3, 3, 12]);
        assert_eq!(c.walk_hits, if round == 0 { 2 } else { 3 });
        let ords = crate::draws::ordinals(f.frame());
        assert_eq!(
            ords.last().map(|o| (o.draw, o.copy)),
            Some((n[0] as u32, n[3] as u32 - 1))
        );
    }
}
