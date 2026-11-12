//! The learner: judging a frame, the learning state of a range and the class of a range bound by one more key.

use super::*;

/// Judges one frame from the bytes at its flush (`pre`), after the GPU finished it (`post`) and at the next flush
/// (`next`). The words are 32 bit; a shorter tail is one word.
pub(crate) fn judge(pre: &[u8], post: &[u8], next: &[u8]) -> Verdict {
    let mut written = false;
    for ((p, q), n) in pre.chunks(4).zip(post.chunks(4)).zip(next.chunks(4)) {
        if p != q {
            written = true;
            if q == n {
                return Verdict::Dirty;
            }
        }
    }
    if written {
        Verdict::Clean
    } else {
        Verdict::Quiet
    }
}

impl Default for Learn {
    fn default() -> Self {
        Self::new()
    }
}

impl Learn {
    pub(crate) fn new() -> Self {
        Self {
            class: Class::Learning,
            clean: 0,
            quiet: 0,
            since: 0,
            checking: false,
            frames: 0,
        }
    }

    /// A range that is only read: its frames are watched.
    pub(super) fn watch() -> Self {
        Self {
            class: Class::Watch,
            ..Self::new()
        }
    }

    pub(super) fn set(&mut self, class: Class) {
        self.class = class;
        self.checking = false;
    }

    pub(crate) fn class(&self) -> Class {
        self.class
    }

    /// Frames in which the range was bound.
    pub(crate) fn frames(&self) -> u64 {
        self.frames
    }

    /// The range is bound in a frame. Starts a check of a renamed range that is due, and ends the pause of an idle one.
    pub(crate) fn tick(&mut self) {
        self.frames += 1;
        match self.class {
            Class::Rename => {
                self.since += 1;
                if self.since >= RECHECK_EVERY && !self.checking {
                    self.checking = true;
                    self.quiet = 0;
                }
            }
            Class::Idle => {
                self.since += 1;
                if self.since >= RECHECK_EVERY {
                    self.class = Class::Learning;
                    self.clean = 0;
                    self.quiet = 0;
                }
            }
            Class::Learning
            | Class::Never
            | Class::Watch
            | Class::Snapshot
            | Class::Merge
            | Class::Conflict => {}
        }
    }

    /// True when the frame that binds the range is judged: it is bound to the guest range and its wait is honest.
    pub(crate) fn sampling(&self) -> bool {
        self.class == Class::Learning || (self.class == Class::Rename && self.checking)
    }

    /// True when the frame that binds the range reads a copy.
    #[cfg(test)]
    pub(crate) fn renaming(&self) -> bool {
        self.class == Class::Rename && !self.checking
    }

    /// Takes the verdict of a frame. Returns the new class when it changed.
    pub(crate) fn observe(&mut self, v: Verdict) -> Option<Class> {
        match (self.class, v) {
            (Class::Learning, Verdict::Dirty) => {
                self.class = Class::Never;
                Some(Class::Never)
            }
            (Class::Learning, Verdict::Clean) => {
                self.quiet = 0;
                self.clean += 1;
                if self.clean >= LEARN_FRAMES {
                    self.class = Class::Rename;
                    self.since = 0;
                    self.checking = false;
                    return Some(Class::Rename);
                }
                None
            }
            (Class::Learning, Verdict::Quiet) => {
                self.quiet += 1;
                if self.quiet >= QUIET_LIMIT {
                    self.class = Class::Idle;
                    self.since = 0;
                    return Some(Class::Idle);
                }
                None
            }
            (Class::Rename, Verdict::Dirty) if self.checking => {
                self.class = Class::Never;
                self.checking = false;
                Some(Class::Never)
            }
            (Class::Rename, Verdict::Clean) if self.checking => {
                self.checking = false;
                self.since = 0;
                None
            }
            (Class::Rename, Verdict::Quiet) if self.checking => {
                // A frame that says nothing does not end the check, but the check does not go on for ever.
                self.quiet += 1;
                if self.quiet >= QUIET_LIMIT {
                    self.checking = false;
                    self.since = 0;
                    self.quiet = 0;
                }
                None
            }
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// What a frame may rename
// ---------------------------------------------------------------------------------------------------------------------

/// The class and masks of a range that is bound by one more key.
pub(super) fn combine(c: &mut Cand, ki: &KeyInfo) {
    use Class::*;
    let (a, b) = (c.learn.class(), ki.class);
    let class = match (a, b) {
        (x, y) if x == y => x,
        (Conflict, _) | (_, Conflict) => Conflict,
        (Merge, Rename) | (Rename, Merge) => {
            note_n(Reason::OverlapKeyDisagree, 1);
            Conflict
        }
        // Reading needs nothing; a range that nothing else needs stays as it is.
        (Snapshot, x) | (x, Snapshot) | (Never, x) | (x, Never) => x,
        (x, _) => x,
    };
    c.learn.set(class);
    c.ro = c.ro && ki.ro;
    union_mask(&mut c.cmask, &ki.cmask);
    union_mask(&mut c.gmask, &ki.gmask);
}
