//! Which elements of a pool table changed since a batch slot last took them.
//!
//! The host keeps one master of each pool table (a map of the non-zero elements) and one copy of the table for each
//! batch slot in flight, because a frame that still runs on the GPU must not see an element that a later frame wrote.
//! A write to the master is noted here. When a frame takes a slot, [`Changes::take`] says which elements to copy into
//! the slot's table: the ones noted since that slot last took them. The log is a plain list of IDs. Each slot has a
//! cursor into it, and the part that every slot has passed is dropped. A slot that fell behind the log (the log had to
//! be cut) gets [`Pending::All`].
//!
//! This is pure logic: it needs no Metal device.

/// What a slot has to copy.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Pending<'a> {
    /// Every element: the log no longer reaches back to the slot's last copy.
    All,
    /// The elements with these IDs, in the order they were noted. An ID may repeat.
    Ids(&'a [u32]),
}

pub(super) struct Changes {
    log: Vec<u32>,
    /// The absolute position of `log[0]`.
    base: u64,
    /// For each slot, the absolute position up to which it has copied.
    cursors: Vec<u64>,
    /// The log is cut when it reaches this many entries.
    cap: usize,
}

impl Changes {
    pub(super) fn new(slots: usize, cap: usize) -> Self {
        Self {
            log: Vec::new(),
            base: 0,
            cursors: vec![0; slots],
            cap: cap.max(1),
        }
    }

    /// Notes that the master element `id` changed.
    pub(super) fn note(&mut self, id: u32) {
        // The last entry may stand for this change only while no slot has passed it.
        let end = self.base + self.log.len() as u64;
        if self.log.last() == Some(&id) && self.cursors.iter().all(|c| *c < end) {
            return;
        }
        self.trim();
        if self.log.len() >= self.cap {
            self.base += self.log.len() as u64;
            self.log.clear();
        }
        self.log.push(id);
    }

    /// What `slot` has to copy now. The slot counts as up to date after this call.
    pub(super) fn take(&mut self, slot: usize) -> Pending<'_> {
        self.trim();
        let end = self.base + self.log.len() as u64;
        let from = std::mem::replace(&mut self.cursors[slot], end);
        if from < self.base {
            Pending::All
        } else {
            Pending::Ids(&self.log[(from - self.base) as usize..])
        }
    }

    /// Drops the part of the log that every slot has passed, once that is at least half of it.
    fn trim(&mut self) {
        let min = self.cursors.iter().copied().min().unwrap_or(self.base);
        let passed = (min.saturating_sub(self.base)) as usize;
        if passed > 0 && passed * 2 >= self.log.len() {
            self.log.drain(..passed);
            self.base += passed as u64;
        }
    }

    /// The entries held. For tests.
    #[cfg(test)]
    pub(super) fn held(&self) -> usize {
        self.log.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(p: Pending<'_>) -> Vec<u32> {
        match p {
            Pending::Ids(i) => i.to_vec(),
            Pending::All => panic!("expected ids"),
        }
    }

    #[test]
    fn a_slot_gets_the_changes_since_it_last_took_them() {
        let mut c = Changes::new(3, 100);
        c.note(5);
        c.note(9);
        assert_eq!(ids(c.take(0)), vec![5, 9]);
        c.note(5);
        assert_eq!(ids(c.take(0)), vec![5]);
        // slot 1 never took anything: it gets all that was noted
        assert_eq!(ids(c.take(1)), vec![5, 9, 5]);
        assert_eq!(ids(c.take(1)), Vec::<u32>::new());
        // slot 2 sees the same history, in order
        assert_eq!(ids(c.take(2)), vec![5, 9, 5]);
    }

    #[test]
    fn an_id_noted_again_after_a_slot_took_it_reaches_that_slot() {
        // publish, sync(0), clear, publish of the same id, sync(0)
        let mut c = Changes::new(3, 100);
        c.note(6);
        assert_eq!(ids(c.take(0)), vec![6]);
        c.note(6);
        assert_eq!(ids(c.take(0)), vec![6]);
        // slot 1 never synced: it still sees the id
        assert_eq!(ids(c.take(1)), vec![6, 6]);
        // a change of the id that one slot took and another did not
        c.note(5);
        let _ = c.take(0);
        c.note(5);
        assert_eq!(ids(c.take(0)), vec![5]);
        assert_eq!(ids(c.take(1)), vec![5, 5]);
    }

    #[test]
    fn a_repeat_of_the_last_id_is_noted_once() {
        let mut c = Changes::new(1, 100);
        c.note(4);
        c.note(4);
        c.note(6);
        c.note(4);
        assert_eq!(ids(c.take(0)), vec![4, 6, 4]);
    }

    #[test]
    fn the_part_every_slot_passed_is_dropped() {
        let mut c = Changes::new(2, 1000);
        for i in 0..10 {
            c.note(i);
        }
        let _ = c.take(0);
        // slot 1 is behind: nothing can go
        c.note(100);
        assert_eq!(c.held(), 11);
        let _ = c.take(1);
        c.note(101);
        assert!(c.held() < 11, "the passed part is gone");
        assert_eq!(ids(c.take(0)), vec![100, 101]);
        assert_eq!(ids(c.take(1)), vec![101]);
    }

    #[test]
    fn a_slot_behind_a_cut_log_copies_everything() {
        let mut c = Changes::new(2, 4);
        for i in 0..4 {
            c.note(i);
        }
        assert_eq!(ids(c.take(0)), vec![0, 1, 2, 3]);
        // the log is full and cut; slot 1 never took any of it
        c.note(10);
        assert_eq!(c.take(1), Pending::All);
        // slot 0 passed everything before the cut
        assert_eq!(ids(c.take(0)), vec![10]);
        // and after All, slot 1 is current
        c.note(11);
        assert_eq!(ids(c.take(1)), vec![11]);
    }

    #[test]
    fn slots_that_rotate_each_frame_keep_the_log_short() {
        let mut c = Changes::new(3, 1 << 20);
        for frame in 0..300u32 {
            for k in 0..5 {
                c.note(frame * 5 + k);
            }
            let _ = c.take(frame as usize % 3);
        }
        assert!(c.held() < 100, "held {}", c.held());
    }
}
