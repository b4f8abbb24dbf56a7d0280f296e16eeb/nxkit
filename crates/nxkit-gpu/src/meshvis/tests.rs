use super::*;

fn att(tex: u32, format: &str, w: u32, h: u32) -> Att {
    Att {
        tex: TexId(tex),
        format: format.to_owned(),
        width: w,
        height: h,
    }
}

fn pass(index: usize, depth: Option<Att>, draws: u32) -> PassInfo {
    PassInfo {
        index,
        colours: vec![att(1, "Rgba8Unorm", 1280, 720)],
        depth,
        load: "Clear".to_owned(),
        clear: Some(1.0),
        draws,
    }
}

fn rec(pass: usize, kind: Kind, mesh: &str, outcome: Outcome) -> DrawRec {
    DrawRec {
        pass,
        kind,
        task: "-".to_owned(),
        mesh: mesh.to_owned(),
        fragment: "f1".to_owned(),
        depth: "LessEqual".to_owned(),
        write: true,
        cull: "Back".to_owned(),
        outcome,
    }
}

#[test]
fn the_first_frames_with_mesh_draws_are_sampled_then_every_120th() {
    assert!(is_sampled(0, 7, false));
    assert!(is_sampled(2, 9, false));
    assert!(!is_sampled(3, 9, false));
    assert!(!is_sampled(500, 119, false));
    assert!(is_sampled(500, 120, false));
    assert!(is_sampled(500, 240, false));
}

#[test]
fn the_knob_samples_every_frame() {
    assert!(is_sampled(1000, 13, true));
}

#[test]
fn draws_with_the_same_state_make_one_group() {
    let p = [pass(2, Some(att(9, "D32Float", 1280, 720)), 5)];
    let d = [
        rec(2, Kind::Mesh, "m1", Outcome::Samples(0)),
        rec(2, Kind::Mesh, "m1", Outcome::Samples(40)),
        rec(2, Kind::Mesh, "m1", Outcome::Samples(7)),
        rec(2, Kind::MeshIndirect, "m1", Outcome::Samples(3)),
        rec(2, Kind::Mesh, "m2", Outcome::Shared),
    ];
    let out = lines(5, &p, &d);
    assert_eq!(out.len(), 5);
    assert_eq!(
        out[0],
        "DIAG meshvis frame 5 pass 2 colour [TexId(1) Rgba8Unorm 1280x720] depth TexId(9) D32Float 1280x720 | mesh draws 5 counted 4"
    );
    assert_eq!(
        out[1],
        "DIAG meshvis frame 5 pass 2 mesh ts - ms m1 fs f1 depth LessEqual write on cull Back draws 3 nonzero 2 samples 47 max 40"
    );
    assert_eq!(
        out[2],
        "DIAG meshvis frame 5 pass 2 mesh-indirect ts - ms m1 fs f1 depth LessEqual write on cull Back draws 1 nonzero 1 samples 3 max 3"
    );
    assert_eq!(
        out[3],
        "DIAG meshvis frame 5 pass 2 mesh ts - ms m2 fs f1 depth LessEqual write on cull Back draws 1 nonzero 0 samples 0 max 0 uncounted 1"
    );
    assert_eq!(
        out[4],
        "DIAG meshvis frame 5: mesh draws 5 counted 4 shared 1 over-limit 0 | draws with samples 3 | samples 50"
    );
}

#[test]
fn another_depth_state_or_cull_makes_another_group() {
    let p = [pass(0, None, 3)];
    let mut b = rec(0, Kind::Mesh, "m", Outcome::Samples(1));
    b.write = false;
    let mut c = rec(0, Kind::Mesh, "m", Outcome::Samples(1));
    c.cull = "None".to_owned();
    let d = [rec(0, Kind::Mesh, "m", Outcome::Samples(1)), b, c];
    let out = lines(1, &p, &d);
    // The pass line, three groups and the summary.
    assert_eq!(out.len(), 5);
    assert!(out[0].contains("depth - |"));
    assert!(out[2].contains("write off"));
    assert!(out[3].contains("cull None"));
}

#[test]
fn the_groups_of_a_frame_are_limited() {
    let p = [pass(0, None, 300)];
    let d: Vec<DrawRec> = (0..MAX_GROUPS + 5)
        .map(|i| rec(0, Kind::Mesh, &format!("m{i}"), Outcome::Samples(1)))
        .collect();
    let out = lines(1, &p, &d);
    // The pass line, the listed groups, the line for the rest and the summary.
    assert_eq!(out.len(), 1 + MAX_GROUPS + 1 + 1);
    assert_eq!(
        out[MAX_GROUPS + 1],
        "DIAG meshvis frame 1: 5 more groups not listed"
    );
}

#[test]
fn over_limit_draws_are_counted_in_the_summary() {
    let p = [pass(0, None, 3)];
    let d = [
        rec(0, Kind::Mesh, "m", Outcome::Samples(2)),
        rec(0, Kind::Mesh, "m", Outcome::OverLimit),
        rec(0, Kind::Mesh, "m", Outcome::NotCounted),
    ];
    let out = lines(8, &p, &d);
    assert_eq!(
        out.last().unwrap(),
        "DIAG meshvis frame 8: mesh draws 3 counted 1 shared 0 over-limit 1 | draws with samples 1 | samples 2"
    );
}

#[test]
fn a_depth_pass_without_mesh_draws_has_its_own_line() {
    let p = [
        pass(1, Some(att(9, "D32Float", 64, 64)), 12),
        pass(2, Some(att(9, "D32Float", 64, 64)), 3),
    ];
    let d = [rec(2, Kind::Mesh, "m", Outcome::Samples(1))];
    let out = lines(4, &p, &d);
    assert_eq!(
        out[0],
        "DIAG meshvis frame 4 pass 1 (no mesh draws) depth TexId(9) load Clear clear 1 draws 12"
    );
    assert!(out[1].starts_with("DIAG meshvis frame 4 pass 2 colour"));
}

#[test]
fn a_load_has_no_clear_value() {
    let mut q = pass(1, Some(att(9, "D32Float", 64, 64)), 2);
    q.load = "Load".to_owned();
    q.clear = None;
    let out = lines(4, &[q], &[]);
    assert_eq!(
        out[0],
        "DIAG meshvis frame 4 pass 1 (no mesh draws) depth TexId(9) load Load clear - draws 2"
    );
}

fn plan_of(n: usize) -> Plan {
    let entries = (0..n)
        .map(|i| Entry {
            pass: 0,
            ord: i as u32 + 10,
            kind: Kind::Mesh,
            task: "-".to_owned(),
            mesh: "m".to_owned(),
            fragment: "-".to_owned(),
        })
        .collect::<Vec<_>>();
    Plan {
        frame: 1,
        passes: vec![pass(0, None, n as u32)],
        live: Mutex::new((0..n).map(|_| Live::default()).collect()),
        entries,
        base: 0,
        limit: 0,
        at: FastMap::default(),
        with_offsets: FastSet::default(),
    }
}

#[test]
fn the_offsets_follow_the_offsets_of_the_game() {
    let mut p = plan_of(3);
    p.bind(4, 100);
    assert_eq!(p.offset(10), Some(32));
    assert_eq!(p.offset(12), Some(48));
    assert_eq!(p.offset(13), None);
    assert!(p.has_pass(0));
    assert!(!p.has_pass(1));
}

#[test]
fn the_room_of_the_buffer_limits_the_offsets() {
    let mut p = plan_of(5);
    p.bind(2, 3);
    assert_eq!(p.offset(12), Some(32));
    assert_eq!(p.offset(13), None);
    for ord in 10..15 {
        p.encoded(ord);
    }
    let out = p.report(&|_| false, &Some);
    assert_eq!(
        out.last().unwrap(),
        "DIAG meshvis frame 1: mesh draws 5 counted 3 shared 0 over-limit 2 | draws with samples 3 | samples 9"
    );
}

#[test]
fn no_room_leaves_every_draw_over_the_limit() {
    let mut p = plan_of(2);
    p.bind(0, 0);
    assert!(!p.has_pass(0));
    let out = p.report(&|_| false, &|_| Some(1));
    assert!(out.last().unwrap().contains("over-limit 2"));
}

#[test]
fn the_plan_wants_at_most_the_maximum() {
    assert_eq!(plan_of(3).wanted(), 3);
    assert_eq!(plan_of(MAX_OFFSETS + 7).wanted(), MAX_OFFSETS);
}

#[test]
fn a_draw_of_the_game_report_is_shared_not_counted() {
    let mut p = plan_of(2);
    p.bind(0, 10);
    p.encoded(10);
    p.encoded(11);
    let out = p.report(&|ord| ord == 10, &|_| Some(5));
    assert_eq!(
        out.last().unwrap(),
        "DIAG meshvis frame 1: mesh draws 2 counted 1 shared 1 over-limit 0 | draws with samples 1 | samples 5"
    );
}

#[test]
fn a_draw_that_was_not_encoded_is_not_counted() {
    let mut p = plan_of(2);
    p.bind(0, 10);
    p.encoded(11);
    let out = p.report(&|_| false, &|_| Some(5));
    assert!(out.last().unwrap().contains("counted 1 shared 0"));
}
