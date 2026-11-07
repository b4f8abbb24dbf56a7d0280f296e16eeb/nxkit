//! The eight controller slots, a port of nxnative `pad_slots.h` (MIT).
//!
//! A slot keeps the name of its last controller after the controller left. A controller that comes back gets its old
//! slot, so player numbers stay where they were.

use crate::npad::PLAYERS;

/// The owner of a slot is a nonzero key. The caller uses the address of its controller object.
#[derive(Debug, Default)]
pub(crate) struct Slots {
    owner: [usize; PLAYERS],
    name: [String; PLAYERS],
}

impl Slots {
    /// The slot of `owner`, if it holds one.
    pub(crate) fn slot_of(&self, owner: usize) -> Option<usize> {
        if owner == 0 {
            return None;
        }
        self.owner.iter().position(|&o| o == owner)
    }

    /// The owner of slot `p`.
    pub(crate) fn owner_of(&self, p: usize) -> Option<usize> {
        self.owner.get(p).copied().filter(|&o| o != 0)
    }

    /// Gives `owner` a slot. A held slot wins. Then a free slot that remembers `name`, then slot 0, then a free slot
    /// with no name, then any free slot. `None` when all eight are taken.
    pub(crate) fn take(&mut self, owner: usize, name: &str) -> Option<usize> {
        if owner == 0 {
            return None;
        }
        if let Some(p) = self.slot_of(owner) {
            return Some(p);
        }
        let this = &*self;
        let is_free = |p: usize| this.owner[p] == 0;
        let p = (0..PLAYERS)
            .find(|&p| is_free(p) && this.name[p] == name)
            .or_else(|| is_free(0).then_some(0))
            .or_else(|| (0..PLAYERS).find(|&p| is_free(p) && this.name[p].is_empty()))
            .or_else(|| (0..PLAYERS).find(|&p| is_free(p)))?;
        self.owner[p] = owner;
        self.name[p] = name.to_owned();
        Some(p)
    }

    /// Frees the slot of `owner`. The slot keeps its name.
    pub(crate) fn free(&mut self, owner: usize) {
        if let Some(p) = self.slot_of(owner) {
            self.owner[p] = 0;
        }
    }

    /// Bit `p` is set when slot `p` is held.
    pub(crate) fn mask(&self) -> u8 {
        (0..PLAYERS).fold(0, |m, p| if self.owner[p] != 0 { m | 1 << p } else { m })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_controller_gets_slot_zero() {
        let mut s = Slots::default();
        assert_eq!(s.take(10, "a"), Some(0));
        assert_eq!(s.take(10, "a"), Some(0));
        assert_eq!(s.take(11, "b"), Some(1));
        assert_eq!(s.mask(), 0b11);
    }

    #[test]
    fn a_returning_controller_gets_its_old_slot() {
        let mut s = Slots::default();
        s.take(10, "a");
        s.take(11, "b");
        s.free(10);
        s.free(11);
        assert_eq!(s.take(21, "b"), Some(1));
        assert_eq!(s.take(20, "a"), Some(0));
    }

    #[test]
    fn a_new_name_prefers_slot_zero_then_an_unnamed_slot() {
        let mut s = Slots::default();
        s.take(10, "a");
        s.free(10);
        assert_eq!(s.take(11, "c"), Some(0));
        assert_eq!(s.take(12, "d"), Some(1));
    }

    #[test]
    fn nine_controllers_do_not_fit() {
        let mut s = Slots::default();
        for i in 1..=8 {
            assert!(s.take(i, &format!("n{i}")).is_some());
        }
        assert_eq!(s.take(9, "n9"), None);
        assert_eq!(s.owner_of(0), Some(1));
    }
}
