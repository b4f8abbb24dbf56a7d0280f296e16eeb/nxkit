//! The slot ring of the Neural Engine path: which set of tensors is being written by the GPU, run by the Neural Engine,
//! ready to be read, or being read. No Metal and no Core ML here, so the rules can be tested.
//!
//! A slot goes `Free -> Filling` (the picture's frame encodes the input into it) `-> Running` (that frame finished on
//! the GPU, the Neural Engine runs) `-> Ready` (the outputs are written) `-> Reading` (the next picture's frame encodes
//! its epilogue from it) `-> Free` (that frame finished on the GPU). Every state carries the number of the picture, so
//! a late callback of an older picture finds the slot taken and does nothing.

/// The state of one slot. `u64` is the number of the picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum State {
    Free,
    Filling(u64),
    Running(u64),
    Ready(u64),
    Reading(u64),
}

/// Why a picture does not use the Neural Engine's result of the picture before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Miss {
    /// This is the first picture of the ring.
    First,
    /// The previous picture's result is not ready (the Neural Engine is late, or it failed, or the picture was not sent).
    Late,
}

/// What the ring decides for a new picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Turn {
    /// The number of the picture.
    pub(super) pic: u64,
    /// The slot the frame writes the input into. `None` when every slot is busy: the picture is not sent.
    pub(super) write: Option<usize>,
    /// The slot of the previous picture's result, when it is ready.
    pub(super) read: Result<usize, Miss>,
}

/// The slots and the picture counter.
#[derive(Debug)]
pub(super) struct Ring {
    slots: Vec<State>,
    pic: u64,
}

impl Ring {
    pub(super) fn new(slots: usize) -> Self {
        Self {
            slots: vec![State::Free; slots],
            pic: 0,
        }
    }

    #[cfg(test)]
    pub(super) fn state(&self, slot: usize) -> State {
        self.slots[slot]
    }

    /// Starts the next picture. Results older than the previous picture are dropped: nobody will read them.
    pub(super) fn begin(&mut self) -> Turn {
        self.pic += 1;
        let pic = self.pic;
        for s in &mut self.slots {
            if matches!(*s, State::Ready(p) if p + 1 < pic) {
                *s = State::Free;
            }
        }
        let read = match self
            .slots
            .iter()
            .position(|s| matches!(s, State::Ready(p) if p + 1 == pic))
        {
            Some(i) => {
                self.slots[i] = State::Reading(pic - 1);
                Ok(i)
            }
            None if pic == 1 => Err(Miss::First),
            None => Err(Miss::Late),
        };
        let write = self.slots.iter().position(|s| *s == State::Free);
        if let Some(i) = write {
            self.slots[i] = State::Filling(pic);
        }
        Turn { pic, write, read }
    }

    /// The frame that wrote the input has finished on the GPU. True when the Neural Engine should run on the slot.
    pub(super) fn filled(&mut self, slot: usize, pic: u64, ok: bool) -> bool {
        if self.slots.get(slot) != Some(&State::Filling(pic)) {
            return false;
        }
        self.slots[slot] = if ok { State::Running(pic) } else { State::Free };
        ok
    }

    /// The Neural Engine has finished (or failed) on the slot.
    pub(super) fn finished(&mut self, slot: usize, pic: u64, ok: bool) {
        if self.slots.get(slot) == Some(&State::Running(pic)) {
            self.slots[slot] = if ok { State::Ready(pic) } else { State::Free };
        }
    }

    /// The frame that read the slot's outputs has finished on the GPU (or was dropped).
    pub(super) fn released(&mut self, slot: usize, pic: u64) {
        if self.slots.get(slot) == Some(&State::Reading(pic)) {
            self.slots[slot] = State::Free;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One picture whose frame and Neural Engine run to the end before the next picture begins.
    fn full_cycle(r: &mut Ring) -> Turn {
        let t = r.begin();
        if let Some(w) = t.write {
            assert!(r.filled(w, t.pic, true));
            r.finished(w, t.pic, true);
        }
        if let Ok(s) = t.read {
            r.released(s, t.pic - 1);
        }
        t
    }

    #[test]
    fn the_first_picture_has_nothing_to_read_but_writes() {
        let mut r = Ring::new(3);
        let t = r.begin();
        assert_eq!(t.read, Err(Miss::First));
        assert_eq!(t.write, Some(0));
        assert_eq!(r.state(0), State::Filling(1));
    }

    #[test]
    fn a_picture_reads_the_result_of_the_one_before() {
        let mut r = Ring::new(3);
        full_cycle(&mut r);
        let t = r.begin();
        assert_eq!(t.read, Ok(0));
        assert_eq!(t.write, Some(1));
        assert_eq!(r.state(0), State::Reading(1));
        r.released(0, 1);
        assert_eq!(r.state(0), State::Free);
    }

    #[test]
    fn a_late_neural_engine_means_a_miss_and_the_result_is_dropped_later() {
        let mut r = Ring::new(3);
        let a = r.begin();
        assert!(r.filled(a.write.unwrap_or(9), 1, true));
        // Picture 2 begins while picture 1 still runs.
        let b = r.begin();
        assert_eq!(b.read, Err(Miss::Late));
        r.finished(0, 1, true);
        assert_eq!(r.state(0), State::Ready(1));
        // Picture 3 does not read picture 1 any more: it wants picture 2.
        let c = r.begin();
        assert_eq!(c.read, Err(Miss::Late));
        // Picture 1's result is dropped, and its slot is free again: picture 3 writes into it.
        assert_eq!(c.write, Some(0));
        assert_eq!(r.state(0), State::Filling(3));
    }

    #[test]
    fn a_failed_frame_or_neural_engine_frees_the_slot() {
        let mut r = Ring::new(3);
        let a = r.begin();
        assert!(!r.filled(a.write.unwrap_or(9), 1, false));
        assert_eq!(r.state(0), State::Free);
        let b = r.begin();
        let w = b.write.unwrap_or(9);
        assert!(r.filled(w, 2, true));
        r.finished(w, 2, false);
        assert_eq!(r.state(w), State::Free);
        assert_eq!(r.begin().read, Err(Miss::Late));
    }

    #[test]
    fn a_full_ring_sends_nothing() {
        let mut r = Ring::new(2);
        let a = r.begin();
        let b = r.begin();
        assert!(a.write.is_some() && b.write.is_some());
        let c = r.begin();
        assert_eq!(c.write, None);
    }

    #[test]
    fn a_stale_callback_does_nothing() {
        let mut r = Ring::new(1);
        let a = r.begin();
        let slot = a.write.unwrap_or(9);
        // The frame of picture 1 is reported twice, then the Neural Engine for a picture that is not in the slot.
        assert!(r.filled(slot, 1, true));
        assert!(!r.filled(slot, 1, true));
        r.finished(slot, 7, true);
        assert_eq!(r.state(slot), State::Running(1));
        r.released(slot, 1);
        assert_eq!(r.state(slot), State::Running(1));
    }

    #[test]
    fn steady_state_reads_every_picture_with_three_slots() {
        let mut r = Ring::new(3);
        full_cycle(&mut r);
        for _ in 0..20 {
            let t = full_cycle(&mut r);
            assert!(t.read.is_ok() && t.write.is_some());
        }
    }
}
