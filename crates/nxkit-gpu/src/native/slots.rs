//! Which picture lives in which set of tensors of the Neural Engine path, and which guest memory holds which picture.
//! No Metal and no Core ML here, so the rules can be tested.
//!
//! A picture is known by its number (`Frame::id` of the video service). It goes `Queued` (decoded, waiting for the
//! feeder) `-> Running` (the input is written and the Neural Engine runs on a slot) `-> Ready` (the outputs are written)
//! `-> Reading` (the GPU frames that make the game's picture from it, counted) `-> Ready` again when the last of them
//! has finished. A `Ready` slot keeps its result, so a picture that the game shows twice still finds it, until a newer
//! picture needs the slot and takes the oldest one. A picture that never got a slot is `Skipped`, with the reason.

use std::collections::VecDeque;

/// The number of a picture.
pub(super) type Pic = u64;

/// Why a picture got no result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Why {
    /// The model or the conversion constants were not there yet.
    Warmup,
    /// Every slot was busy, or the picture's result was taken for a newer one.
    Busy,
    /// The picture is not the size of the model's input.
    Size,
    /// The conversion or the Neural Engine failed.
    Failed,
}

/// Why a picture has no result to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Miss {
    /// The table has never heard of the picture.
    Unknown,
    /// The Neural Engine path has the picture but is not done with it.
    Late,
    /// The picture got no result.
    Skipped(Why),
    /// The result was made with other conversion constants than the ones the game binds now.
    Params,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fate {
    Queued,
    Running(usize),
    Ready(usize),
    Reading { slot: usize, readers: u32 },
    Skipped(Why),
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    pic: Pic,
    /// The conversion constants the input was made with (set when the picture starts).
    epoch: u32,
    fate: Fate,
}

/// The pictures the table remembers. Older ones are forgotten, except those that hold a slot.
const KEEP: usize = 16;

/// The pictures and the slots they hold.
#[derive(Debug)]
pub(super) struct Table {
    entries: VecDeque<Entry>,
    free: Vec<usize>,
}

impl Table {
    pub(super) fn new(slots: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            free: (0..slots).rev().collect(),
        }
    }

    fn find(&mut self, pic: Pic) -> Option<&mut Entry> {
        self.entries.iter_mut().rev().find(|e| e.pic == pic)
    }

    /// A decoded picture. Pictures the table has no room for are forgotten, oldest first.
    pub(super) fn queued(&mut self, pic: Pic) {
        self.entries.push_back(Entry {
            pic,
            epoch: 0,
            fate: Fate::Queued,
        });
        while self.entries.len() > KEEP {
            match self.entries.front().map(|e| e.fate) {
                Some(Fate::Running(_) | Fate::Reading { .. }) | None => break,
                Some(Fate::Ready(s)) => self.free.push(s),
                Some(Fate::Queued | Fate::Skipped(_)) => {}
            }
            self.entries.pop_front();
        }
    }

    /// The picture's input is to be written with conversion constants `epoch`: gives the slot to write, a free one or
    /// else the oldest result that nobody reads (its picture is `Skipped(Busy)` from now on). `None` when every slot
    /// is running or being read; the picture is `Skipped(Busy)`.
    pub(super) fn start(&mut self, pic: Pic, epoch: u32) -> Option<usize> {
        self.find(pic)?;
        let slot = self.free.pop().or_else(|| {
            let e = self
                .entries
                .iter_mut()
                .find(|e| matches!(e.fate, Fate::Ready(_)))?;
            let Fate::Ready(s) = e.fate else { return None };
            e.fate = Fate::Skipped(Why::Busy);
            Some(s)
        });
        let e = self.find(pic)?;
        match slot {
            Some(s) => {
                e.epoch = epoch;
                e.fate = Fate::Running(s);
            }
            None => e.fate = Fate::Skipped(Why::Busy),
        }
        slot
    }

    /// The picture gets no result.
    pub(super) fn skip(&mut self, pic: Pic, why: Why) {
        if let Some(e) = self.find(pic) {
            e.fate = Fate::Skipped(why);
        }
    }

    /// The Neural Engine is done with the picture on `slot` (or failed, or the input could not be written).
    pub(super) fn finished(&mut self, slot: usize, pic: Pic, ok: bool) {
        let Some(e) = self.find(pic) else {
            return;
        };
        if e.fate != Fate::Running(slot) {
            return;
        }
        if ok {
            e.fate = Fate::Ready(slot);
        } else {
            e.fate = Fate::Skipped(Why::Failed);
            self.free.push(slot);
        }
    }

    /// A GPU frame is to read the picture's result, made with constants `epoch`. Gives the slot, ready now.
    pub(super) fn take(&mut self, pic: Pic, epoch: u32) -> Result<usize, Miss> {
        let e = self.find(pic).ok_or(Miss::Unknown)?;
        match e.fate {
            Fate::Queued | Fate::Running(_) => Err(Miss::Late),
            Fate::Skipped(w) => Err(Miss::Skipped(w)),
            Fate::Ready(_) | Fate::Reading { .. } if e.epoch != epoch => Err(Miss::Params),
            Fate::Ready(slot) => {
                e.fate = Fate::Reading { slot, readers: 1 };
                Ok(slot)
            }
            Fate::Reading { slot, readers } => {
                e.fate = Fate::Reading {
                    slot,
                    readers: readers + 1,
                };
                Ok(slot)
            }
        }
    }

    /// A GPU frame that took the picture has finished (or was dropped). The last reader leaves the result `Ready`.
    pub(super) fn release(&mut self, pic: Pic) {
        let Some(e) = self.find(pic) else {
            return;
        };
        if let Fate::Reading { slot, readers } = e.fate {
            e.fate = if readers > 1 {
                Fate::Reading {
                    slot,
                    readers: readers - 1,
                }
            } else {
                Fate::Ready(slot)
            };
        }
    }

    #[cfg(test)]
    fn free_slots(&self) -> usize {
        self.free.len()
    }

    #[cfg(test)]
    fn held(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| {
                matches!(
                    e.fate,
                    Fate::Running(_) | Fate::Ready(_) | Fate::Reading { .. }
                )
            })
            .count()
    }
}

/// The guest memory the game read pictures into: the host addresses of the luma and chroma planes of the most recent
/// pictures. The game's dispatch names its input by those addresses.
#[derive(Debug, Default)]
pub(super) struct Binds {
    list: VecDeque<(u64, u64, Pic)>,
}

/// The reads the table remembers.
const BINDS: usize = 16;

impl Binds {
    /// The game read `pic` into the planes at `luma` and `chroma`. An older read into the same luma address is replaced:
    /// the game reuses its buffer, and the dispatch that follows belongs to the newest picture.
    pub(super) fn bind(&mut self, luma: u64, chroma: u64, pic: Pic) {
        self.list.retain(|b| b.0 != luma);
        self.list.push_back((luma, chroma, pic));
        while self.list.len() > BINDS {
            self.list.pop_front();
        }
    }

    /// The picture whose luma was read into `luma`, whatever the chroma address.
    pub(super) fn find_luma(&self, luma: u64) -> Option<Pic> {
        self.list.iter().find(|b| b.0 == luma).map(|b| b.2)
    }

    /// The reads, oldest first: luma and chroma host addresses and the picture.
    pub(super) fn all(&self) -> Vec<(u64, u64, Pic)> {
        self.list.iter().copied().collect()
    }

    /// The addresses of the last reads, for a diagnostic.
    pub(super) fn recent(&self) -> String {
        let tail: Vec<String> = self
            .list
            .iter()
            .rev()
            .take(3)
            .map(|b| format!("{:#x}/{:#x}", b.0, b.1))
            .collect();
        if tail.is_empty() {
            "the game has read no picture".into()
        } else {
            format!("the game read pictures into {}", tail.join(" "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture that is decoded, fed and finished.
    fn ready(t: &mut Table, pic: Pic, epoch: u32) -> usize {
        t.queued(pic);
        let s = t.start(pic, epoch).unwrap_or(99);
        t.finished(s, pic, true);
        s
    }

    #[test]
    fn a_picture_goes_from_queued_through_running_to_ready() {
        let mut t = Table::new(2);
        assert_eq!(t.take(1, 0), Err(Miss::Unknown));
        t.queued(1);
        assert_eq!(t.take(1, 0), Err(Miss::Late));
        let s = t.start(1, 0);
        assert!(s.is_some());
        assert_eq!(t.take(1, 0), Err(Miss::Late));
        t.finished(s.unwrap_or(9), 1, true);
        assert_eq!(t.take(1, 0), s.ok_or(Miss::Unknown));
    }

    #[test]
    fn a_result_is_read_by_the_picture_it_belongs_to_and_by_no_other() {
        let mut t = Table::new(3);
        let a = ready(&mut t, 10, 0);
        let b = ready(&mut t, 11, 0);
        assert_ne!(a, b);
        assert_eq!(t.take(11, 0), Ok(b));
        assert_eq!(t.take(10, 0), Ok(a));
        assert_eq!(t.take(12, 0), Err(Miss::Unknown));
    }

    #[test]
    fn a_result_stays_until_the_last_reader_is_done_and_a_newer_picture_needs_the_slot() {
        let mut t = Table::new(1);
        let s = ready(&mut t, 1, 0);
        assert_eq!(t.take(1, 0), Ok(s));
        assert_eq!(t.take(1, 0), Ok(s));
        // Two frames read it: a newer picture finds no slot while they do.
        t.queued(2);
        assert_eq!(t.start(2, 0), None);
        assert_eq!(t.take(2, 0), Err(Miss::Skipped(Why::Busy)));
        t.release(1);
        t.release(1);
        // Ready and unread: the next picture takes the slot, and the first is skipped from now on.
        t.queued(3);
        assert_eq!(t.start(3, 0), Some(s));
        assert_eq!(t.take(1, 0), Err(Miss::Skipped(Why::Busy)));
        // Running slots are never taken.
        t.queued(4);
        assert_eq!(t.start(4, 0), None);
    }

    #[test]
    fn a_shown_twice_picture_finds_its_result_again() {
        let mut t = Table::new(2);
        let s = ready(&mut t, 1, 0);
        assert_eq!(t.take(1, 0), Ok(s));
        t.release(1);
        assert_eq!(t.take(1, 0), Ok(s));
        t.release(1);
        assert_eq!(t.held(), 1);
    }

    #[test]
    fn the_oldest_unread_result_is_the_one_that_goes() {
        let mut t = Table::new(2);
        let a = ready(&mut t, 1, 0);
        let b = ready(&mut t, 2, 0);
        t.queued(3);
        assert_eq!(t.start(3, 0), Some(a));
        assert_eq!(t.take(2, 0), Ok(b));
    }

    #[test]
    fn constants_that_changed_make_the_old_results_unusable() {
        let mut t = Table::new(2);
        ready(&mut t, 1, 5);
        assert_eq!(t.take(1, 6), Err(Miss::Params));
        assert!(t.take(1, 5).is_ok());
        assert_eq!(t.take(1, 6), Err(Miss::Params));
    }

    #[test]
    fn a_failed_run_frees_the_slot_and_names_the_reason() {
        let mut t = Table::new(1);
        t.queued(1);
        let s = t.start(1, 0).unwrap_or(9);
        t.finished(s, 1, false);
        assert_eq!(t.take(1, 0), Err(Miss::Skipped(Why::Failed)));
        assert_eq!(t.free_slots(), 1);
    }

    #[test]
    fn a_late_callback_of_another_picture_does_nothing() {
        let mut t = Table::new(1);
        t.queued(1);
        let s = t.start(1, 0).unwrap_or(9);
        t.finished(s, 7, true);
        t.finished(s + 1, 1, true);
        assert_eq!(t.take(1, 0), Err(Miss::Late));
        t.release(1);
        t.finished(s, 1, true);
        assert_eq!(t.take(1, 0), Ok(s));
        // A second completion of the same run does not change a result that is being read.
        t.finished(s, 1, false);
        assert_eq!(t.take(1, 0), Ok(s));
    }

    #[test]
    fn a_skipped_picture_keeps_its_reason() {
        let mut t = Table::new(1);
        t.queued(1);
        t.skip(1, Why::Warmup);
        assert_eq!(t.take(1, 0), Err(Miss::Skipped(Why::Warmup)));
        t.skip(9, Why::Size);
        assert_eq!(t.take(9, 0), Err(Miss::Unknown));
    }

    #[test]
    fn old_pictures_are_forgotten_and_their_slots_come_back() {
        let mut t = Table::new(2);
        ready(&mut t, 1, 0);
        for p in 2..=(KEEP as u64 + 1) {
            t.queued(p);
            t.skip(p, Why::Warmup);
        }
        assert_eq!(t.take(1, 0), Err(Miss::Unknown));
        assert!(t.free_slots() >= 1);
        assert_eq!(t.held(), 0);
        // A picture that is being read is never forgotten.
        let mut t = Table::new(1);
        let s = ready(&mut t, 1, 0);
        assert_eq!(t.take(1, 0), Ok(s));
        for p in 2..=(KEEP as u64 + 5) {
            t.queued(p);
        }
        assert_eq!(t.take(1, 0), Ok(s));
    }

    #[test]
    fn the_planes_name_the_newest_picture_read_into_them() {
        let mut b = Binds::default();
        b.bind(0x1000, 0x9000, 1);
        b.bind(0x2000, 0xa000, 2);
        assert_eq!(b.find_luma(0x1000), Some(1));
        b.bind(0x1000, 0x9000, 3);
        assert_eq!(b.find_luma(0x1000), Some(3));
        assert_eq!(b.find_luma(0x2000), Some(2));
        assert_eq!(b.find_luma(0x3000), None);
    }

    #[test]
    fn the_binds_are_bounded_and_the_diagnostic_names_the_last_reads() {
        let mut b = Binds::default();
        assert!(b.recent().contains("no picture"));
        for i in 0..40u64 {
            b.bind(0x1000 + i * 0x100, 0x9000, i);
        }
        assert_eq!(b.find_luma(0x1000), None);
        assert_eq!(b.find_luma(0x1000 + 39 * 0x100), Some(39));
        assert!(b.recent().contains("0x3700/0x9000"), "{}", b.recent());
    }
}
