//! Tests of [`super`].

use super::*;
use nxkit_nvn::frame::Span;

fn dispatch() -> Cmd {
    Cmd::Dispatch {
        reads: Span::default(),
        writes: Span::default(),
    }
}

fn bind(index: u32, source: Source, writes: bool) -> Binding {
    Binding {
        index,
        source,
        writes,
    }
}

fn step(buffers: Vec<Binding>, textures: Vec<Binding>) -> Step {
    Step {
        kernel: "k",
        groups: [1; 3],
        threads: [1; 3],
        threadgroup_bytes: 0,
        buffers,
        textures,
    }
}

#[test]
fn a_run_is_found_between_barriers() {
    let cmds = vec![
        dispatch(),
        dispatch(),
        Cmd::Barrier(1),
        dispatch(),
        Cmd::Sync,
        dispatch(),
        dispatch(),
    ];
    assert_eq!(run_commands(&cmds, Run { start: 1, len: 3 }), Some((1, 5)));
    assert_eq!(run_commands(&cmds, Run { start: 0, len: 5 }), Some((0, 6)));
}

#[test]
fn a_copy_inside_the_run_refuses_it() {
    let cmds = vec![
        dispatch(),
        Cmd::Copy {
            src: None,
            dst: None,
        },
        dispatch(),
    ];
    assert_eq!(run_commands(&cmds, Run { start: 0, len: 2 }), None);
    // The same copy after the run is fine.
    assert_eq!(run_commands(&cmds, Run { start: 0, len: 1 }), Some((0, 0)));
}

#[test]
fn a_run_past_the_dispatches_is_not_found() {
    let cmds = vec![dispatch(), dispatch()];
    assert_eq!(run_commands(&cmds, Run { start: 1, len: 2 }), None);
    assert_eq!(run_commands(&cmds, Run { start: 2, len: 1 }), None);
    assert_eq!(run_commands(&cmds, Run { start: 1, len: 1 }), Some((1, 1)));
}

#[test]
fn the_blob_starts_before_the_bound_address() {
    assert_eq!(blob_start(0x10_0000, 0x1300), Some(0xfed00));
    assert_eq!(blob_start(0x100, 0x1300), None);
}

#[test]
fn a_barrier_goes_between_a_write_and_the_read_of_it() {
    let a = Source::Buffer(0);
    let b = Source::Buffer(1);
    let w = Source::Buffer(2);
    let steps = vec![
        step(vec![bind(0, a, true)], vec![]),
        step(
            vec![bind(0, a, false), bind(1, w, false), bind(2, b, true)],
            vec![],
        ),
        // Reads `a` again and writes nothing the earlier step touched: the barrier before step 1 cleared the set.
        step(vec![bind(0, a, false), bind(1, w, false)], vec![]),
        // Reads what step 1 wrote.
        step(vec![bind(0, b, false)], vec![]),
    ];
    assert_eq!(barrier_flags(&steps), vec![false, true, false, true]);
}

#[test]
fn a_write_after_a_read_needs_a_barrier() {
    let a = Source::Buffer(0);
    let steps = vec![
        step(vec![bind(0, a, false)], vec![]),
        step(vec![bind(0, a, true)], vec![]),
    ];
    assert_eq!(barrier_flags(&steps), vec![false, true]);
}

#[test]
fn independent_steps_share_a_wave() {
    let steps = vec![
        step(vec![bind(0, Source::Buffer(0), true)], vec![]),
        step(
            vec![bind(0, Source::Buffer(1), true)],
            vec![bind(0, Source::Game(GameResource::Texture(0)), false)],
        ),
    ];
    assert_eq!(barrier_flags(&steps), vec![false, false]);
}

#[test]
fn a_game_texture_written_then_sampled_needs_a_barrier() {
    let out = Source::Game(GameResource::Output);
    let steps = vec![
        step(vec![], vec![bind(0, out, true)]),
        step(vec![], vec![bind(0, out, false)]),
    ];
    assert_eq!(barrier_flags(&steps), vec![false, true]);
    assert_eq!(
        written_resources(&Plan {
            buffers: vec![],
            steps
        }),
        vec![GameResource::Output]
    );
}

#[test]
fn the_hash_follows_the_bytes_and_their_split() {
    let a = hash_blobs(&[vec![1, 2, 3], vec![4]]);
    assert_eq!(a, hash_blobs(&[vec![1, 2, 3], vec![4]]));
    assert_ne!(a, hash_blobs(&[vec![1, 2, 3], vec![5]]));
    assert_ne!(a, hash_blobs(&[vec![1, 2], vec![3, 4]]));
}

#[test]
fn used_resources_lists_each_game_resource_once() {
    let g = |r| bind(0, Source::Game(r), false);
    let plan = Plan {
        buffers: vec![],
        steps: vec![
            step(
                vec![g(GameResource::Push(0))],
                vec![g(GameResource::Texture(0))],
            ),
            step(vec![], vec![g(GameResource::Texture(0))]),
        ],
    };
    assert_eq!(
        used_resources(&plan),
        vec![GameResource::Push(0), GameResource::Texture(0)]
    );
    assert!(uses_resource(&plan, GameResource::Texture(0)));
    assert!(!uses_resource(&plan, GameResource::Output));
}

#[test]
fn a_prepared_run_skips_its_dispatches_and_anchors_the_last() {
    let p = Prepared {
        entry: Arc::new(Entry {
            plan: Plan {
                buffers: vec![],
                steps: vec![],
            },
            buffers: vec![],
            pipelines: vec![],
            barrier_before: vec![],
            written: vec![],
        }),
        name: "t",
        first: 3,
        anchor: 5,
        textures: vec![],
        pushes: vec![],
        reads_later: vec![],
        compare: None,
    };
    assert!(!p.skips(2) && p.skips(3) && p.skips(5) && !p.skips(6));
    assert!(p.is_anchor(5) && !p.is_anchor(4));
}

#[test]
fn the_plan_books_its_writes_in_the_pass_of_the_anchor() {
    use nxkit_nvn::analyse::Analyser;
    use nxkit_nvn::passes::{PassKind, PassPlan};
    use nxkit_nvn::resource::ResourceTable;

    let ctx = match nxkit_metal::Context::system_default() {
        Ok(c) => c,
        Err(_) => return,
    };
    let mut frame = Frame::new();
    for i in 0..3 {
        if i > 0 {
            frame.cmds.push(Cmd::Barrier(1));
        }
        frame.cmds.push(dispatch());
    }
    let mut plan = PassPlan::default();
    Analyser::new()
        .analyse(&frame, &ResourceTable::new(), &mut plan)
        .unwrap();
    let computes: Vec<_> = plan
        .passes
        .iter()
        .enumerate()
        .filter(|(_, p)| p.kind == PassKind::Compute)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(computes.len(), 3);
    let desc = TextureDesc::compute_2d(4, 4, nxkit_metal::Format::Rgba8Unorm);
    let tex = Texture::new(&ctx, desc).unwrap();
    let p = Prepared {
        entry: Arc::new(Entry {
            plan: Plan {
                buffers: vec![],
                steps: vec![],
            },
            buffers: vec![],
            pipelines: vec![],
            barrier_before: vec![],
            written: vec![GameResource::Output],
        }),
        name: "t",
        first: 0,
        anchor: 2,
        textures: vec![
            (GameResource::Output, tex.clone(), desc, TexId(7)),
            (GameResource::Texture(0), tex, desc, TexId(8)),
        ],
        pushes: vec![],
        reads_later: vec![],
        compare: None,
    };
    assert_eq!(p.writes(), vec![TexId(7)]);
    let ords = crate::draws::ordinals(&frame);
    let mut images = vec![crate::images::PassImages::default(); plan.passes.len()];
    book_writes(&p, &frame, &plan, &ords, &mut images);
    for (i, x) in images.iter().enumerate() {
        let want: Vec<TexId> = if i == computes[2] {
            vec![TexId(7)]
        } else {
            vec![]
        };
        assert_eq!(x.writes, want, "pass {i}");
    }
}
