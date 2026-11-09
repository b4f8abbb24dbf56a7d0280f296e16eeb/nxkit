//! More tests of the pass analysis: buffer copies, compute passes, dropped passes and the reuse of the analyser.

use super::*;

#[test]
fn a_buffer_copy_does_not_cut_the_render_pass() {
    let mut f = Fx::new();
    let c = f.tex(RGBA8);
    f.rec.set_render_targets(&[c], None);
    f.rec.draw(paint());
    f.rec.copy(None, None);
    f.rec.draw(paint());
    f.run().unwrap();
    assert_eq!(f.plan.passes.len(), 1);
    assert_eq!(f.plan.passes[0].kind, PassKind::Render);
    assert_eq!(f.plan.passes[0].cmds, 1..4);
    assert_eq!(f.plan.stats.buffer_copies_absorbed, 1);
}

#[test]
fn a_buffer_copy_keeps_a_barrier_merge_possible() {
    let mut f = Fx::new();
    let c = f.tex(RGBA8);
    f.rec.set_render_targets(&[c], None);
    f.rec.draw(paint());
    f.rec.barrier(2);
    f.rec.copy(None, None);
    f.rec.draw(paint());
    f.run().unwrap();
    assert_eq!(f.plan.passes.len(), 1);
    assert_eq!(f.plan.stats.render_passes, 1);
}

#[test]
fn a_buffer_copy_does_not_hide_a_read_of_an_attachment() {
    let mut f = Fx::new();
    let c = f.tex(RGBA8);
    f.rec.set_render_targets(&[c], None);
    f.rec.draw(paint());
    f.rec.barrier(0x10);
    f.rec.copy(None, None);
    f.rec.bind_texture(0, Some(c));
    f.rec.draw(paint());
    f.run().unwrap();
    assert_eq!(f.plan.passes.len(), 2);
    assert_eq!(f.plan.passes[1].begin, Boundary::Barrier(0x10));
}

#[test]
fn a_copy_with_a_texture_still_cuts_the_render_pass() {
    let mut f = Fx::new();
    let (c, other) = (f.tex(RGBA8), f.tex(RGBA8));
    f.rec.set_render_targets(&[c], None);
    f.rec.draw(paint());
    f.rec.copy(Some(other), None);
    f.rec.draw(paint());
    f.rec.copy(None, Some(other));
    f.rec.draw(paint());
    f.run().unwrap();
    let kinds: Vec<_> = f.plan.passes.iter().map(|p| p.kind).collect();
    assert_eq!(
        kinds,
        [
            PassKind::Render,
            PassKind::Copy,
            PassKind::Render,
            PassKind::Copy,
            PassKind::Render
        ]
    );
    assert_eq!(f.plan.stats.buffer_copies_absorbed, 0);
}

#[test]
fn compute_passes_around_a_buffer_copy_share_one_pass_in_order() {
    let mut f = Fx::new();
    let (a, b) = (f.tex(RGBA8), f.tex(RGBA8));
    f.rec.bind_texture(0, Some(a));
    f.rec.dispatch();
    f.rec.copy(None, None);
    f.rec.bind_texture(0, Some(b));
    f.rec.dispatch();
    f.run().unwrap();
    assert_eq!(f.plan.passes.len(), 1);
    let p = &f.plan.passes[0];
    assert_eq!(p.kind, PassKind::Compute);
    assert_eq!(p.cmds, 0..3);
    assert_eq!(f.plan.reads_of(p), [a, b]);
}

#[test]
fn a_barrier_between_dispatches_keeps_two_compute_passes() {
    let mut f = Fx::new();
    f.rec.dispatch();
    f.rec.barrier(1);
    f.rec.dispatch();
    f.run().unwrap();
    assert_eq!(f.plan.passes.len(), 2);
    assert_eq!(f.plan.passes[0].begin, Boundary::Start);
    assert_eq!(f.plan.passes[1].begin, Boundary::Barrier(1));
}

#[test]
fn a_render_pass_between_dispatches_keeps_them_apart() {
    let mut f = Fx::new();
    let c = f.tex(RGBA8);
    f.rec.dispatch();
    f.rec.set_render_targets(&[c], None);
    f.rec.draw(paint());
    f.rec.dispatch();
    f.run().unwrap();
    let kinds: Vec<_> = f.plan.passes.iter().map(|p| p.kind).collect();
    assert_eq!(
        kinds,
        [PassKind::Compute, PassKind::Render, PassKind::Compute]
    );
}

#[test]
fn the_boundary_counters_name_the_reasons() {
    let mut f = Fx::new();
    let (c, d) = (f.tex(RGBA8), f.tex(RGBA8));
    f.rec.dispatch();
    f.rec.barrier(1);
    f.rec.dispatch();
    f.rec.set_render_targets(&[c], None);
    f.rec.draw(paint());
    f.rec.set_render_targets(&[d], None);
    f.rec.draw(paint());
    f.rec.dispatch();
    f.run().unwrap();
    let s = f.plan.stats;
    assert_eq!(s.compute_begins.start, 1);
    assert_eq!(s.compute_begins.barrier, 1);
    assert_eq!(s.compute_begins.foreign, 1);
    assert_eq!(s.render_begins.targets, 2);
    assert_eq!(s.render_begins.total(), 2);
}

/// The frame of `f` with the draws that repeat the lists of the draw before pointing at the same span.
fn share_repeated_lists(f: &Frame) -> Frame {
    let mut out = f.clone();
    let mut prev: Option<(DrawState, crate::frame::Span)> = None;
    for c in &mut out.cmds {
        match c {
            crate::frame::Cmd::Draw { state, reads } => {
                if let Some((s, r)) = prev
                    && s == *state
                    && f.list(r) == f.list(*reads)
                {
                    *reads = r;
                }
                prev = Some((*state, *reads));
            }
            _ => prev = None,
        }
    }
    out
}

#[test]
fn draws_that_repeat_the_previous_draw_give_the_same_plan() {
    let mut f = Fx::new();
    let (a, b, d, s) = (f.tex(RGBA8), f.tex(RGBA8), f.tex(D32F), f.tex(RGBA8));
    let blend = DrawState {
        color_write: 1,
        blend: true,
        ..DrawState::default()
    };
    f.rec.set_render_targets(&[a], Some(d));
    f.rec.clear_color(0);
    f.rec.clear_depth_stencil();
    for _ in 0..3 {
        f.rec.draw(depth_draw());
    }
    f.rec.bind_texture(0, Some(s));
    for _ in 0..3 {
        f.rec.draw(depth_draw());
    }
    for _ in 0..2 {
        f.rec.draw(blend);
    }
    f.rec.discard(1, true);
    for _ in 0..2 {
        f.rec.draw(blend);
    }
    f.rec.draw(depth_draw());
    f.rec.draw(depth_draw());
    f.rec.barrier(2);
    f.rec.draw(depth_draw());
    f.rec.draw(depth_draw());
    f.rec.set_scissor_full(false);
    f.rec.clear_color(0);
    f.rec.draw(depth_draw());
    f.rec.draw(depth_draw());
    f.rec.set_scissor_full(true);
    f.rec.clear_color(0);
    f.rec.draw(depth_draw());
    f.rec.set_render_targets(&[b], None);
    f.rec.bind_texture(1, Some(a));
    for _ in 0..3 {
        f.rec.draw(paint());
    }
    f.rec.present(b);
    f.frame.clear();
    f.frame.append(f.rec.stream());
    let shared = share_repeated_lists(&f.frame);
    assert!(
        shared.cmds != f.frame.cmds,
        "the frame has repeated draws to share"
    );
    let (mut plain, mut coalesced) = (PassPlan::new(), PassPlan::new());
    Analyser::new()
        .analyse(&f.frame, &f.table, &mut plain)
        .unwrap();
    Analyser::new()
        .analyse(&shared, &f.table, &mut coalesced)
        .unwrap();
    assert!(plain.passes.len() > 2);
    assert_eq!(format!("{plain:?}"), format!("{coalesced:?}"));
}

#[test]
fn an_analyser_that_ran_before_gives_the_plan_of_a_fresh_one() {
    let mut f = Fx::new();
    let (a, b, d) = (f.tex(RGBA8), f.tex(RGBA8), f.tex(D32F));
    f.rec.set_render_targets(&[a], Some(d));
    f.rec.draw(depth_draw());
    f.rec.bind_texture(0, Some(b));
    f.rec.set_render_targets(&[b], None);
    f.rec.draw(paint());
    f.rec.present(b);
    f.run().unwrap();
    // the second frame uses fewer textures, and the host marks one of them as frame-local in between
    f.table.set_frame_local(d, true).unwrap();
    f.table.finalize(a).unwrap();
    let c = f.tex(RGBA8);
    f.rec.begin();
    f.rec.set_render_targets(&[c], Some(d));
    f.rec.draw(depth_draw());
    f.run().unwrap();
    let mut fresh = PassPlan::new();
    Analyser::new()
        .analyse(&f.frame, &f.table, &mut fresh)
        .unwrap();
    assert_eq!(format!("{:?}", f.plan), format!("{fresh:?}"));
    assert_eq!(f.plan.attachments[1].store, StoreAction::DontCare);
    assert_eq!(f.plan.attachments[0].store, StoreAction::Store);
}

#[test]
fn a_render_pass_with_only_a_discard_is_dropped() {
    let mut f = Fx::new();
    let (c, d) = (f.tex(RGBA8), f.tex(RGBA8));
    f.rec.set_render_targets(&[c], None);
    f.rec.discard(1, false);
    f.rec.set_render_targets(&[d], None);
    f.rec.draw(paint());
    f.run().unwrap();
    assert_eq!(f.plan.passes.len(), 1);
    assert_eq!(f.plan.attachments.len(), 1);
    assert_eq!(f.plan.attachments[0].tex, d);
    assert_eq!(f.plan.stats.empty_dropped, 1);
    assert_eq!(f.plan.stats.render_begins.total(), 1);
}

#[test]
fn passes_around_a_dropped_pass_merge() {
    let mut f = Fx::new();
    let (c, d) = (f.tex(RGBA8), f.tex(RGBA8));
    f.rec.set_render_targets(&[c], None);
    f.rec.draw(paint());
    f.rec.set_render_targets(&[d], None);
    f.rec.discard(1, false);
    f.rec.set_render_targets(&[c], None);
    f.rec.draw(depth_free_blend());
    f.run().unwrap();
    assert_eq!(f.plan.stats.render_passes, 1);
    assert_eq!(f.plan.stats.merged_after_targets, 1);
    assert_eq!(f.plan.stats.render_begins.total(), 1);
}

fn depth_free_blend() -> DrawState {
    DrawState {
        blend: true,
        ..paint()
    }
}

#[test]
fn a_dropped_pass_lets_compute_passes_group() {
    let mut f = Fx::new();
    let c = f.tex(RGBA8);
    f.rec.dispatch();
    f.rec.set_render_targets(&[c], None);
    f.rec.discard(1, false);
    f.rec.dispatch();
    f.run().unwrap();
    assert_eq!(f.plan.passes.len(), 1);
    assert_eq!(f.plan.passes[0].kind, PassKind::Compute);
}

/// Runs `frames` frames of a texture that is cleared first and drawn, and returns the last plan's first attachment.
fn scratch_frames(f: &mut Fx, c: TexId, frames: u32, sample_first: Option<TexId>) {
    for _ in 0..frames {
        f.rec.begin();
        if let Some(d) = sample_first {
            f.rec.bind_texture(0, Some(c));
            f.rec.set_render_targets(&[d], None);
            f.rec.draw(paint());
            f.rec.bind_texture(0, None);
        }
        f.rec.set_render_targets(&[c], None);
        f.rec.clear_color(0);
        f.rec.draw(paint());
        f.run().unwrap();
    }
}

#[test]
fn a_texture_that_starts_dead_for_many_frames_stops_being_stored() {
    let mut f = Fx::new();
    let c = f.tex(RGBA8);
    scratch_frames(&mut f, c, 10, None);
    assert_eq!(f.plan.attachments[0].store, StoreAction::Store);
    scratch_frames(&mut f, c, 130, None);
    let a = f.plan.attachments[0];
    assert_eq!(
        (a.load, a.store),
        (LoadAction::Clear, StoreAction::DontCare)
    );
    assert!(!a.memoryless);
    assert_eq!(f.plan.stats.stores_learned_dead, 1);
    assert_eq!(f.plan.stats.bytes_learned_dead, COLOR_BYTES);
}

#[test]
fn a_texture_that_a_frame_reads_before_it_clears_keeps_its_store() {
    let mut f = Fx::new();
    let (c, d) = (f.tex(RGBA8), f.tex(RGBA8));
    scratch_frames(&mut f, c, 140, Some(d));
    let a = *f.plan.attachments.iter().find(|a| a.tex == c).unwrap();
    assert_eq!(a.store, StoreAction::Store);
    assert_eq!(f.plan.stats.stores_learned_dead, 0);
}

#[test]
fn the_merge_report_counts_default_loads() {
    let mut f = Fx::new();
    let c = f.tex(RGBA8);
    f.rec.set_render_targets(&[c], None);
    f.rec.draw(paint());
    f.run().unwrap();
    let r = crate::passmerge::MergeReport::of(&f.frame, &f.plan);
    assert_eq!(
        (r.render, r.one_draw, r.empty, r.load_default),
        (1, 1, 0, 1)
    );
}
