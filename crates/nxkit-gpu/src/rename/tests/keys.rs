//! Tests of the cost of a big frame, the keys of bindings and the wait knobs.

use super::*;

/// The cost of a frame of the renamer on the CPU thread, for a frame of 3000 draws with 4 storage buffers, 4 uniform buffers
/// and 2 vertex streams each, 650 candidate ranges of which 24 are renamed. Run with
/// `cargo test --release -p nxkit-gpu --lib renamer_cost -- --ignored --nocapture`.
#[test]
#[ignore]
fn renamer_cost_of_a_big_frame() {
    use crate::detail::{Stage, StreamDetail, UboDetail};
    let Ok(ctx) = nxkit_metal::Context::system_default() else {
        return;
    };
    let mem = Arc::new(nxkit_metal::GuestMemory::allocate(&ctx, 1 << 28).unwrap());
    let guest = nxkit_metal::GuestSpace::over(&mem).unwrap();
    let base = guest.address_map().gpu_base();
    let renamer = Renamer::new(&ctx, guest);
    let keys: Vec<Arc<str>> = (0..60).map(|i| Arc::from(format!("k{i}"))).collect();
    let mut detail = FrameDetail::default();
    for n in 0..3000u64 {
        let mut d = crate::walk::empty_draw();
        d.program.vertex = keys[(n % 60) as usize].clone();
        d.program.fragment = Some(keys[((n + 7) % 60) as usize].clone());
        for slot in 0..4u32 {
            // 650 ranges of 16 KiB to 40 KiB, spread over the memory.
            let i = (n * 4 + u64::from(slot)) % 650;
            d.sbufs.push(UboDetail {
                stage: if slot < 2 {
                    Stage::Vertex
                } else {
                    Stage::Fragment
                },
                slot: slot % 2,
                address: base + 0x10_0000 + i * 0x1_0000,
                size: 0x4000 + (i as u32 % 4) * 0x2000,
                snapshot: None,
            });
            d.ubos.push(UboDetail {
                stage: Stage::Vertex,
                slot,
                address: base + 0x2000_0000 + (n % 300) * 0x1000 + u64::from(slot) * 0x100,
                size: 0x100,
                snapshot: None,
            });
        }
        for index in 0..2u32 {
            d.streams.push(StreamDetail {
                index,
                address: base + 0x3000_0000 + (n % 500) * 0x2000,
                size: 0x2000,
                stride: 16,
                divisor: 0,
            });
        }
        detail.draws.push(Arc::new(d));
    }
    // Slot 0 of each stage may be written.
    let rw_of = |_: &str, _: bool, slot: u32| slot == 0;
    renamer.note_rw_with(&detail, None, &rw_of);
    {
        let mut st = lock(&renamer.state);
        let n = st.cands.len();
        // Make 24 of the written candidates renamed: 8 each of Snapshot, Merge and Rename.
        let mut addrs: Vec<u64> = st
            .cands
            .iter()
            .filter(|(_, c)| !c.ro)
            .map(|(a, _)| *a)
            .collect();
        addrs.sort_unstable();
        for (k, a) in addrs.iter().take(24).enumerate() {
            let c = st.cands.get_mut(a).unwrap();
            c.learn.set(match k % 3 {
                0 => Class::Rename,
                1 => Class::Merge,
                _ => Class::Snapshot,
            });
            c.hits = 2;
            c.cmask = vec![1];
            c.gmask = vec![2];
        }
        eprintln!("renamer cost: {n} candidates");
    }
    let frames = 200;
    let t = std::time::Instant::now();
    // The encoder learns whether the programs write, then the flush places the bindings and sends it the few that it could
    // not place.
    renamer.note_rw_with(&detail, None, &rw_of);
    let check = {
        let mut st = lock(&renamer.state);
        let (bindings, _) = st.bindings(&detail);
        st.adopt(&bindings)
    };
    eprintln!("  bindings left for the encoder: {}", check.len());
    for _ in 0..frames {
        renamer.note_rw_with(&detail, Some(&check), &rw_of);
    }
    let note = t.elapsed();
    let t = std::time::Instant::now();
    for k in 0..frames {
        let c = renamer.at_flush(&detail, k + 1);
        std::hint::black_box(c);
    }
    let flush = t.elapsed();
    let time = |name: &str, f: &dyn Fn()| {
        let t = std::time::Instant::now();
        for _ in 0..frames {
            f();
        }
        eprintln!(
            "  {name}: {:.3} ms",
            t.elapsed().as_secs_f64() * 1e3 / frames as f64
        );
    };
    time("bindings", &|| {
        std::hint::black_box(lock(&renamer.state).bindings(&detail));
    });
    let (bs, _) = lock(&renamer.state).bindings(&detail);
    eprintln!("  distinct bindings: {}", bs.len());
    time("adopt", &|| {
        lock(&renamer.state).adopt(&bs);
    });
    time("others", &|| {
        std::hint::black_box(others(&detail));
    });
    let o = others(&detail);
    let ranges: Vec<(u64, u32)> = (0..24)
        .map(|i| (base + 0x10_0000 + i * 0x1_0000, 0x4000))
        .collect();
    time("blocked_flags", &|| {
        std::hint::black_box(blocked_flags(&ranges, o.iter().copied()));
    });
    eprintln!(
        "renamer cost per frame: note_rw {:.3} ms, at_flush {:.3} ms",
        note.as_secs_f64() * 1e3 / frames as f64,
        flush.as_secs_f64() * 1e3 / frames as f64
    );
}

#[test]
fn a_new_address_of_a_known_key_gets_the_class_and_masks_at_once() {
    let k = key(7, 1, 0x40);
    let mut st = taught(k);
    assert_eq!(st.keys[&k].class, Class::Merge);
    // The ring comes round at another address, with a size of the same block.
    st.adopt(&[binding(SUM + 0x1000, 0x30, k)]);
    let c = &st.cands[&(SUM + 0x1000)];
    assert_eq!(c.learn.class(), Class::Merge);
    assert!(c.derived);
    assert_eq!(
        (c.cmask.clone(), c.gmask.clone()),
        (vec![0b0010], vec![0b0100])
    );
    // The next flush captures it as a merge range, with no learning frame.
    let g = Guest(RefCell::new(vec![0; 0x2000]));
    let f = st.flush(
        1,
        &[(SUM + 0x1000, 0x30)],
        &|r| vec![false; r.len()],
        &|| Vec::new(),
        &|a, b| g.read(a, b),
    );
    assert_eq!(f.renamed.len(), 1);
    assert_eq!(f.renamed[0].kind, Kind::Merge);
    assert!(!f.sampling);
    // A binding of another program or slot is not taught.
    st.adopt(&[binding(SUM + 0x2000, 0x40, key(8, 1, 0x40))]);
    assert!(!st.cands.contains_key(&(SUM + 0x2000)));
}

#[test]
fn a_range_that_a_snapshot_binding_also_reads_stays_a_merge_range() {
    let (km, ks) = (key(1, 1, 0x40), key(2, 2, 0x40));
    let mut st = taught(km);
    st.keys.insert(
        ks,
        KeyInfo {
            class: Class::Snapshot,
            ro: true,
            cmask: Vec::new(),
            gmask: Vec::new(),
            grown: 0,
        },
    );
    st.adopt(&[
        binding(SUM + 0x1000, 0x40, km),
        binding(SUM + 0x1000, 0x40, ks),
    ]);
    let c = &st.cands[&(SUM + 0x1000)];
    assert_eq!(c.learn.class(), Class::Merge);
    assert_eq!(c.keys, vec![km, ks]);
}

#[test]
fn bindings_that_need_a_merge_and_a_renamed_range_cannot_be_combined() {
    let (km, kr) = (key(1, 1, 0x40), key(2, 2, 0x40));
    let mut st = taught(km);
    st.keys.insert(
        kr,
        KeyInfo {
            class: Class::Rename,
            ro: false,
            cmask: Vec::new(),
            gmask: Vec::new(),
            grown: 0,
        },
    );
    let before = nxkit_nvn::diag::total(Reason::OverlapKeyDisagree);
    st.adopt(&[
        binding(SUM + 0x1000, 0x40, km),
        binding(SUM + 0x1000, 0x40, kr),
    ]);
    assert_eq!(st.cands[&(SUM + 0x1000)].learn.class(), Class::Conflict);
    assert!(nxkit_nvn::diag::total(Reason::OverlapKeyDisagree) > before);
    // The frame waits in full.
    let g = Guest(RefCell::new(vec![0; 0x2000]));
    let f = st.flush(
        1,
        &[(SUM + 0x1000, 0x40)],
        &|r| vec![false; r.len()],
        &|| Vec::new(),
        &|a, b| g.read(a, b),
    );
    assert!(f.conflict && f.renamed.is_empty());
}

#[test]
fn a_range_that_is_never_for_its_own_reasons_does_not_unteach_the_key() {
    let k = key(1, 1, 0x40);
    let mut st = taught(k);
    st.adopt(&[binding(SUM + 0x1000, 0x40, k)]);
    // The other address is found in a pool of raw stores and demoted.
    assert!(st.demote(SUM + 0x1000));
    st.push_keys(SUM + 0x1000);
    assert_eq!(st.keys[&k].class, Class::Merge);
    // A binding that a shader writes does unteach a snapshot key.
    let ks = key(5, 1, 0x40);
    st.keys.insert(
        ks,
        KeyInfo {
            class: Class::Snapshot,
            ro: true,
            cmask: Vec::new(),
            gmask: Vec::new(),
            grown: 0,
        },
    );
    st.adopt(&[binding(SUM + 0x3000, 0x40, ks)]);
    st.candidate(SUM + 0x3000, 0x40);
    assert_eq!(st.keys[&ks].class, Class::Never);
}

#[test]
fn the_distinct_bindings_of_a_frame_are_found_once_and_keyed_by_block() {
    let mut detail = FrameDetail::default();
    let prog: Arc<str> = Arc::from("prog");
    for size in [100u32, 100, 200] {
        let mut d = crate::walk::empty_draw();
        d.program.vertex = prog.clone();
        d.sbufs.push(crate::detail::UboDetail {
            stage: Stage::Vertex,
            slot: 1,
            address: SUM,
            size,
            snapshot: None,
        });
        detail.draws.push(Arc::new(d));
    }
    let mut st = State::default();
    let (b, ranges) = st.bindings(&detail);
    assert_eq!(ranges, vec![(SUM, 100), (SUM, 200)]);
    // Two sizes, one key.
    assert_eq!(b.len(), 2);
    assert_eq!(b[0].key, b[1].key);
    assert_eq!(b[0].key.size, 256);
}

#[test]
fn a_range_that_passed_the_check_against_other_uses_is_trusted_for_a_while() {
    let g = Guest(RefCell::new(vec![0; 0x400]));
    let mut st = State::default();
    st.watch_read_only(SUM, 0x40);
    st.cands.get_mut(&SUM).unwrap().learn.set(Class::Snapshot);
    let flush = |st: &mut State, others: &BlockedBy| {
        let fence = st.frame + 1;
        st.flush(fence, &[(SUM, 0x40)], others, &|| Vec::new(), &|a, b| {
            g.read(a, b)
        })
    };
    // The check is made until the range passed it CLEAR_RUN times; a use that overlaps resets the count.
    for _ in 0..CLEAR_RUN {
        assert_eq!(flush(&mut st, &|r| vec![false; r.len()]).renamed.len(), 1);
    }
    assert_eq!(st.cands[&SUM].clear_run, CLEAR_RUN);
    let f = flush(&mut st, &|_| panic!("the check is skipped"));
    assert_eq!(f.renamed.len(), 1);
    // Once in 60 flushes it is made again, and a use that overlaps now stops the copy.
    while !(st.frame + 1).is_multiple_of(60) {
        flush(&mut st, &|_| panic!("the check is skipped"));
    }
    let f = flush(&mut st, &|r| vec![true; r.len()]);
    assert!(f.renamed.is_empty());
    assert_eq!(st.cands[&SUM].clear_run, 0);
}

#[test]
fn the_wait_knobs_force_a_full_wait_for_the_class_that_they_name() {
    let g = Guest(RefCell::new(vec![0; 0x400]));
    // (the class, whether it is only read, the bit of the knob)
    for (class, ro, bit) in [
        (Class::Never, false, 0u8),
        (Class::Learning, false, 0),
        (Class::Idle, false, 0),
        (Class::Watch, true, 1),
        (Class::Merge, false, 2),
        (Class::Snapshot, true, 3),
        (Class::Rename, false, 4),
    ] {
        let mut st = State::default();
        st.candidate(SUM, 0x40);
        let c = st.cands.get_mut(&SUM).unwrap();
        c.learn.set(class);
        c.ro = ro;
        c.rest_until = u64::MAX;
        for force in [1u8 << bit, 0b11111 & !(1 << bit), 0] {
            st.force = force;
            let f = st.flush(
                st.frame + 1,
                &[(SUM, 0x40)],
                &|r| vec![false; r.len()],
                &|| Vec::new(),
                &|a, b| g.read(a, b),
            );
            assert_eq!(
                f.forced,
                if force >> bit & 1 != 0 { 1 << bit } else { 0 },
                "{class:?} {force:#b}"
            );
        }
    }
}
