//! The state that a queue keeps from one submit to the next.

use crate::cmdbuf::CommandHandle;
use crate::frame::Cmd;
use crate::front_tests::{CMD, bind, fe, record, targets, tex, with_pools};
use crate::record::Record;
use crate::tic::TextureView;

#[test]
fn the_state_that_a_submit_leaves_is_inherited_by_the_next() {
    let mut f = fe();
    let (c, d) = (tex(&mut f, 0x10_0000), tex(&mut f, 0x20_0000));
    let (t_c, t_d) = (targets(&f, &[c]), targets(&f, &[d]));
    let set_c = record(&mut f, CMD, &[t_c]);
    let set_d = record(&mut f, CMD + 0x1000, &[t_d]);
    let draw = record(&mut f, CMD + 0x2000, &[Record::Draw]);
    assert!(f.queue_state(0).is_none());
    f.submit(set_c).unwrap();
    assert_eq!(f.queue_state(0).unwrap().targets.color[0], Some(c));
    f.submit(draw).unwrap();
    f.submit(set_d).unwrap();
    assert_eq!(f.queue_state(0).unwrap().targets.color[0], Some(d));
    f.submit(draw).unwrap();
    // the draw was decoded under both attachments, the other submits hit the cache
    assert!(f.cache().entry(draw.0, 1).is_some() && f.cache().entry(draw.0, 2).is_none());
    f.submit(set_c).unwrap();
    f.submit(draw).unwrap();
    assert_eq!((f.cache_stats().misses, f.cache_stats().hits), (4, 2));
}

#[test]
fn a_texture_bound_by_one_submit_is_sampled_by_the_next() {
    let mut f = fe();
    with_pools(&mut f);
    let a = tex(&mut f, 0x10_0000);
    f.register_texture(1, a, TextureView::identity()).unwrap();
    let set = record(&mut f, CMD, &[bind(2, 1)]);
    let draw = record(&mut f, CMD + 0x1000, &[Record::Draw]);
    f.submit(draw).unwrap();
    f.submit(set).unwrap();
    f.submit(draw).unwrap();
    let reads: Vec<_> = f
        .frame()
        .cmds
        .iter()
        .map(|c| match c {
            Cmd::Draw { reads, .. } => f.frame().list(*reads).to_vec(),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(reads, [vec![], vec![a]]);
}

#[test]
fn queues_do_not_share_state() {
    let mut f = fe();
    let c = tex(&mut f, 0x10_0000);
    let t_c = targets(&f, &[c]);
    let set = record(&mut f, CMD, &[t_c]);
    f.submit_on(1, set).unwrap();
    assert_eq!(f.queue_state(1).unwrap().targets.color[0], Some(c));
    assert!(f.queue_state(0).is_none());
    f.drop_queue(1);
    assert!(f.queue_state(1).is_none());
}

#[test]
fn a_finalised_texture_leaves_the_state_of_the_queue() {
    let mut f = fe();
    let c = tex(&mut f, 0x10_0000);
    let t_c = targets(&f, &[c]);
    let set = record(&mut f, CMD, &[t_c]);
    f.submit(set).unwrap();
    f.finalize_texture(c).unwrap();
    assert_eq!(f.queue_state(0).unwrap().targets.color[0], None);
}

#[test]
fn a_failed_submit_keeps_the_state() {
    let mut f = fe();
    let c = tex(&mut f, 0x10_0000);
    let t_c = targets(&f, &[c]);
    let set = record(&mut f, CMD, &[t_c]);
    f.submit(set).unwrap();
    assert!(f.submit(CommandHandle(CMD + 0x3000)).is_err());
    assert_eq!(f.queue_state(0).unwrap().targets.color[0], Some(c));
}

#[test]
fn a_present_goes_behind_the_submitted_commands() {
    let mut f = fe();
    let c = tex(&mut f, 0x10_0000);
    assert!(f.frame_is_empty());
    let draw = record(&mut f, CMD, &[Record::Draw]);
    f.submit(draw).unwrap();
    f.present(c);
    let cmds = &f.frame().cmds;
    assert!(matches!(cmds[0], Cmd::Draw { .. }));
    assert!(matches!(cmds[1], Cmd::Present(t) if t == c));
}
