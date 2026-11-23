//! Walks of parts of a recording (`Walker::submit_range`), as the queue gate runs a submit that it split at a wait.

use super::*;

fn wait(value: u32) -> Record {
    let [lo, hi] = halves(GPU + 0x7000);
    ext(ExtCode::WaitEvent, 0, &[lo, hi, 0, value])
}

/// A wait inside a called recording splits the walk: the part up to the wait and the part from it give the draws of the
/// whole walk, in order, with the state of the queue carried over.
#[test]
fn the_parts_of_a_split_recording_walk_as_the_whole() {
    let mut whole = Rig::new();
    let mut parts = Rig::new();
    let mut handles = [0u64; 2];
    for (k, r) in [&mut whole, &mut parts].into_iter().enumerate() {
        let mut inner = vec![draw(), wait(3), viewport(80.0), draw()];
        inner.splice(0..0, model(UBO_AT, 1));
        let sub = r.put(&inner);
        let mut top = vec![
            program(PROG),
            viewport(160.0),
            draw(),
            Record::Call(sub),
            draw(),
        ];
        top.splice(0..0, model(UBO_AT + 0x100, 0));
        handles[k] = r.put(&top);
    }
    let (all, _) = whole.frame(&[handles[0]]);

    let mut at = Vec::new();
    nxkit_nvn::decode::queue_marks_at(&parts.mem, handles[1], &mut at).unwrap();
    assert_eq!(at.len(), 1);
    let place = at[0].1.clone();
    assert_eq!(place.len(), 2, "the wait is in the call: {place:x?}");
    parts.w.submit_range(
        &mut parts.mem,
        1,
        handles[1],
        &[handles[1]],
        Some(place.clone()),
    );
    let first = parts.w.marks().draws;
    parts
        .w
        .submit_range(&mut parts.mem, 1, handles[1], &place, None);
    let split = parts.w.take();

    assert_eq!(first, 3, "the draws before the wait");
    assert_eq!(split.draws.len(), all.draws.len());
    for (a, b) in all.draws.iter().zip(&split.draws) {
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
    }
}
