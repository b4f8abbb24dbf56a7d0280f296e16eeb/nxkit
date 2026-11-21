//! Tests of the streamed archive: group opening, draining and grouping stability.

use super::*;

fn inputs(n: usize) -> StreamInputs {
    StreamInputs {
        lib_dir: PathBuf::from("/nonexistent"),
        chain: "c".into(),
        pairs: (0..n)
            .map(|i| (format!("v{i}"), format!("f{}", i % 7)))
            .collect(),
        computes: Vec::new(),
        extras: Vec::new(),
        exacts: Vec::new(),
        zero: false,
        group_target: 8,
        limits: Limits::new(1, 1, 0),
    }
}

#[test]
fn groups_open_when_their_shaders_are_done() {
    let inp = inputs(100);
    let s = Stream::new(&inp);
    assert!(s.groups() > 1);
    let order = s.key_order();
    assert_eq!(order.len(), 107);
    let rx = s.rx.lock().unwrap_or_else(PoisonError::into_inner).take();
    let Some(rx) = rx else { panic!("receiver") };
    // finish the shaders of the first group in order: exactly that group opens first
    let first = s.groups.lock().unwrap_or_else(PoisonError::into_inner)[0]
        .pairs
        .clone();
    let keys: HashSet<String> = first
        .iter()
        .flat_map(|(v, f)| [v.clone(), f.clone()])
        .collect();
    for k in &keys {
        s.done(k);
    }
    assert_eq!(rx.try_recv().ok(), Some(0));
    s.finish();
    let rest: Vec<usize> = rx.try_iter().collect();
    assert!(!rest.contains(&0));
    assert!(rest.len() + 1 >= s.groups() - 1);
}

#[test]
fn a_compute_group_opens_when_its_shaders_are_done() {
    let mut inp = inputs(0);
    inp.computes = (0..20).map(|i| format!("k{i}_b")).collect();
    let s = Stream::new(&inp);
    assert_eq!(s.groups(), 2);
    let rx = s.rx.lock().unwrap_or_else(PoisonError::into_inner).take();
    let Some(rx) = rx else { panic!("receiver") };
    // the shader step's keys have no `_b`. It does not build k15: that group counts it as done
    let built: HashSet<String> = (0..20)
        .filter(|i| *i != 15)
        .map(|i| format!("k{i}"))
        .collect();
    s.building(&built);
    for i in 16..19 {
        s.done(&format!("k{i}"));
    }
    assert!(rx.try_recv().is_err());
    s.done("k19");
    assert_eq!(rx.try_recv().ok(), Some(1));
    for i in 0..14 {
        s.done(&format!("k{i}"));
    }
    assert!(rx.try_recv().is_err());
    s.done("k14");
    assert_eq!(rx.try_recv().ok(), Some(0));
}

#[test]
fn shaders_not_built_do_not_block() {
    let inp = inputs(40);
    let s = Stream::new(&inp);
    let rx = s.rx.lock().unwrap_or_else(PoisonError::into_inner).take();
    let Some(rx) = rx else { panic!("receiver") };
    // only the vertex shaders are built: every group waits for them alone
    let built: HashSet<String> = (0..40).map(|i| format!("v{i}")).collect();
    s.building(&built);
    assert!(rx.try_recv().is_err());
    for k in &built {
        s.done(k);
    }
    assert_eq!(rx.try_iter().count(), s.groups());
}

#[test]
fn drains_every_item_on_all_threads() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    let (tx, rx) = channel();
    let (now, peak, sum) = (
        AtomicUsize::new(0),
        AtomicUsize::new(0),
        AtomicUsize::new(0),
    );
    let work = |i: usize| {
        let n = now.fetch_add(1, Ordering::SeqCst) + 1;
        peak.fetch_max(n, Ordering::SeqCst);
        sum.fetch_add(i, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(20));
        now.fetch_sub(1, Ordering::SeqCst);
    };
    for i in 0..24 {
        let _ = tx.send(i);
    }
    drop(tx);
    drain(rx, 4, &work);
    assert_eq!(sum.load(Ordering::SeqCst), (0..24).sum());
    assert!(peak.load(Ordering::SeqCst) <= 4);
    assert!(peak.load(Ordering::SeqCst) > 1);
}

#[test]
fn grouping_is_stable_under_insertion() {
    let a = Stream::new(&inputs(200));
    let mut more = inputs(200);
    more.pairs.push(("vX".into(), "fX".into()));
    let b = Stream::new(&more);
    let set = |s: &Stream| -> HashSet<Vec<(String, String)>> {
        s.groups
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|g| g.pairs.clone())
            .collect()
    };
    let (sa, sb) = (set(&a), set(&b));
    // all groups but the one that took the new pair (and a neighbour split by the cap) are unchanged
    assert!(sa.intersection(&sb).count() + 2 >= sa.len());
}
