//! The random walks, the speed measurement and the tests of the key and the queue.

use super::*;

/// A small deterministic generator.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

/// One random record that sets or reads state, from the kinds that the walker follows.
fn random_record(r: &mut Lcg, callees: &[u64]) -> Record {
    const OTHER: u64 = GPU + 0xa000;
    let buffer = UBO_AT + 0x40 * r.next(4);
    match r.next(26) {
        0 => program(if r.next(2) == 0 { PROG } else { OTHER }),
        1 => viewport(10.0 + r.next(3) as f32),
        2 => ubo(r.next(3) as u16, buffer, 64),
        3 => vertex_buffer(r.next(3) as u16, GPU + 0x2000 + 0x100 * r.next(2), 256),
        4 => attribute(r.next(4) as u32, 0x0a + r.next(2) as u32, r.next(3) as u32),
        5 => stream(r.next(3) as u32, 4 * (1 + r.next(3) as u32)),
        6 => blend(r.next(3) as u32, 1 + r.next(3) as u32),
        7 => stencil_ref(1 + r.next(3) as u32, r.next(3) as u32),
        8 => Record::BindTexture {
            slot: r.next(70) as u8,
            handle: r.next(3),
        },
        9 => deferred(r.next(40) as u8, 1 + r.next(3) as u8, SRC),
        10 => deferred(
            60 + r.next(8) as u8,
            1 + r.next(3) as u8,
            SRC + 8 * r.next(3),
        ),
        11 => update(buffer, 64, 4 * r.next(8) as u32, r.next(100) as u32),
        12 => dispatch(),
        13 => clear(),
        14 => ext(
            ExtCode::Setting,
            Setting::DepthClamp as u16,
            &[r.next(2) as u32],
        ),
        15 => ext(ExtCode::Scissor, 0, &[0, 0, 5 + r.next(3) as u32, 6]),
        16 => ext(ExtCode::State, 1, &[r.next(256) as u32]),
        17 => ext(ExtCode::State, 5, &[r.next(4) as u32, 0]),
        18 => ext(
            ExtCode::Setting,
            Setting::BlendColor as u16,
            &[r.next(2) as u32, 0, 0, 0],
        ),
        19 => ext(
            ExtCode::Setting,
            Setting::StencilMask as u16,
            &[3, r.next(3) as u32],
        ),
        20 | 21 if !callees.is_empty() => {
            Record::Call(callees[r.next(callees.len() as u64) as usize])
        }
        22 => ext(
            ExtCode::Setting,
            Setting::CopyRowStride as u16,
            &[64 * r.next(3) as u32, 0],
        ),
        23 => ext(
            ExtCode::CopyBufferToTexture,
            0,
            &[1, 0, 2, 0, 0, 0, 0, 0, 4, 4, 1],
        ),
        _ => draw(),
    }
}

#[test]
fn random_recordings_walk_the_same_with_and_without_templates() {
    let mut total = 0;
    for seed in 0..300u64 {
        let mut p = Pair::new(seed % 2 == 1);
        let mut r = Lcg(seed);
        for rig in [&mut p.cached, &mut p.plain] {
            rig.w.program_set(GPU + 0xa000, &keys(5, 6));
        }
        let mut handles: Vec<u64> = Vec::new();
        for _ in 0..4 {
            let new: Vec<u64> = (0..6)
                .map(|_| {
                    let recs: Vec<Record> = (0..2 + r.next(10))
                        .map(|_| random_record(&mut r, &handles))
                        .collect();
                    p.put(&recs)
                })
                .collect();
            handles.extend(new);
        }
        let tops: Vec<u64> = (0..3)
            .map(|_| {
                let recs: Vec<Record> = [program(PROG)]
                    .into_iter()
                    .chain((0..8).map(|_| random_record(&mut r, &handles)))
                    .collect();
                p.put(&recs)
            })
            .collect();
        for _ in 0..25 {
            let values: Vec<u8> = (0..48).map(|_| r.next(4) as u8 * 3).collect();
            if r.next(3) == 0 {
                p.write(SRC, &values);
            }
            if r.next(4) == 0 {
                p.write(HOST + 0x1000 + 4 * r.next(16), &[r.next(200) as u8; 4]);
            }
            total += p.frame(&tops).1.walk_hits;
        }
    }
    assert!(total > 1000, "{total} hits");
}

/// A frame of 1500 models of about 20 records and 2 draws each, walked with and without templates. Run with
/// `cargo test -p nxkit-gpu --release walk_speed -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement"]
fn walk_speed() {
    let time = |cache_on: bool| {
        let mut rig = Rig::new();
        rig.mem = VecMem::new(HOST, 16 << 20);
        rig.w.verify = false;
        rig.w.cache_on = cache_on;
        let models: Vec<u64> = (0..1500)
            .map(|i| {
                let mut m = vec![deferred(0, 3, SRC), viewport(50.0 + (i % 3) as f32)];
                m.extend(model(UBO_AT + 0x40 * (i % 7), 2));
                m.extend([blend(0, 2), stencil_ref(1, 3), ubo(1, UBO_AT, 64), draw()]);
                rig.put(&m)
            })
            .collect();
        let mut top = vec![program(PROG)];
        top.extend(models.iter().map(|m| Record::Call(*m)));
        let top = rig.put(&top);
        let mut best = u128::MAX;
        for _ in 0..10 {
            let start = std::time::Instant::now();
            rig.frame(&[top]);
            best = best.min(start.elapsed().as_micros());
        }
        best
    };
    let (plain, cached) = (time(false), time(true));
    eprintln!("walk of 1500 models: {plain} us without templates, {cached} us with");
}

#[test]
fn the_cache_can_be_cleared_by_memory_writes_from_another_thread() {
    let q = Arc::new(Invalidations::default());
    let q2 = q.clone();
    std::thread::spawn(move || q2.push(10, 20)).join().unwrap();
    let mut out = Vec::new();
    q.drain(&mut out);
    assert_eq!(out, [(10, 20)]);
    q.drain(&mut out);
    assert_eq!(out.len(), 1);
}

#[test]
fn a_key_matches_states_with_the_same_needed_values() {
    let mut needs = DRAW_READS;
    needs.add(&Bits::range(0, 6));
    let mut a = State::default();
    a.texture[5] = 7;
    a.viewport[2] = 4.0;
    let key = Key::of(&needs, &a, 0);
    assert!(key.matches(&a, 0));
    assert!(key.matches(&a.clone(), 0));
    // A field that the key does not need can differ.
    let other = Needs::of(&[sc::VIEWPORT]);
    let narrow = Key::of(&other.0, &a, 0);
    let mut b = a.clone();
    b.blend_color[0] = 1.0;
    assert!(narrow.matches(&b, 0));
    assert!(!key.matches(&b, 0));
    b.blend_color[0] = 0.0;
    b.viewport[2] = 5.0;
    assert!(!narrow.matches(&b, 0));
    let mut b = a.clone();
    b.ubo[1][3] = (0x10, 4);
    assert!(!key.matches(&b, 0));
    assert!(
        !Key::of(&needs, &a, 1 << 5).matches(&a, 0),
        "the provenance of a texture is part of the key"
    );
    assert!(Key::of(&needs, &a, 1 << 5).matches(&a, 1 << 5));
}

struct Needs(Bits);

impl Needs {
    fn of(groups: &[usize]) -> Self {
        Self(groups.iter().fold(Bits::NONE, |mut b, g| {
            b.set(scalar_bit(*g));
            b
        }))
    }
}
