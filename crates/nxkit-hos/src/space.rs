//! The kernel's model of the guest address space: a map of blocks, each with a state, a permission and attributes.
//!
//! The model is what `QueryMemory` answers. The host does the real reservation (R18). The map covers the 39-bit
//! address space of a process. A block is a range of pages with the same properties. A write splits blocks at its
//! ends and joins equal neighbours, so the map stays as small as the guest's use of it. A block that records the
//! source of a `MapMemory` is never joined, because `UnmapMemory` has to find it again.

use std::collections::BTreeMap;

/// The end of the address space of a 64-bit process on Horizon (39 bits).
pub(crate) const SPACE_END: u64 = 1 << 39;

/// Memory states as `QueryMemory` reports them (the low byte of Horizon's state word).
pub(crate) mod state {
    pub(crate) const FREE: u32 = 0;
    pub(crate) const CODE: u32 = 3;
    pub(crate) const CODE_DATA: u32 = 4;
    pub(crate) const NORMAL: u32 = 5;
    pub(crate) const SHARED: u32 = 6;
    pub(crate) const STACK: u32 = 0xB;
    pub(crate) const THREAD_LOCAL: u32 = 0xC;
    pub(crate) const INACCESSIBLE: u32 = 0x10;
}

/// Memory permissions, one bit each for read, write and execute.
pub(crate) mod perm {
    pub(crate) const NONE: u32 = 0;
    pub(crate) const R: u32 = 1;
    pub(crate) const RW: u32 = 3;
    pub(crate) const RX: u32 = 5;
}

/// Memory attributes.
pub(crate) mod attr {
    pub(crate) const LOCKED: u32 = 1;
    pub(crate) const UNCACHED: u32 = 8;
    /// Set once by `SetMemoryAttribute`, never cleared. The host does nothing for it.
    pub(crate) const PERMISSION_LOCKED: u32 = 0x10;
}

/// A range of pages with the same properties.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Block {
    pub start: u64,
    /// The end of the range. It is zero for the block that reaches the end of the 64-bit space.
    pub end: u64,
    pub state: u32,
    pub perm: u32,
    pub attr: u32,
    /// The source address when the block was made by `MapMemory`, else zero.
    pub src: u64,
}

impl Block {
    pub(crate) fn size(&self) -> u64 {
        self.end.wrapping_sub(self.start)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Entry {
    end: u64,
    state: u32,
    perm: u32,
    attr: u32,
    src: u64,
}

/// The block map. Every address below [`SPACE_END`] is in exactly one block.
pub(crate) struct MemoryMap {
    map: BTreeMap<u64, Entry>,
}

impl MemoryMap {
    /// A map in which all memory is inaccessible.
    pub(crate) fn new() -> MemoryMap {
        let all = Entry {
            end: SPACE_END,
            state: state::INACCESSIBLE,
            perm: perm::NONE,
            attr: 0,
            src: 0,
        };
        MemoryMap {
            map: BTreeMap::from([(0, all)]),
        }
    }

    /// The block that holds `addr`. An address past the space gives the block that runs to the end of the 64-bit space.
    pub(crate) fn query(&self, addr: u64) -> Block {
        if addr >= SPACE_END {
            return Block {
                start: SPACE_END,
                end: 0,
                state: state::INACCESSIBLE,
                perm: perm::NONE,
                attr: 0,
                src: 0,
            };
        }
        match self.map.range(..=addr).next_back() {
            Some((&start, e)) => Block {
                start,
                end: e.end,
                state: e.state,
                perm: e.perm,
                attr: e.attr,
                src: e.src,
            },
            None => Block {
                start: 0,
                end: SPACE_END,
                state: state::INACCESSIBLE,
                perm: perm::NONE,
                attr: 0,
                src: 0,
            },
        }
    }

    /// The blocks that overlap `start..end`, whole, in address order.
    pub(crate) fn blocks(&self, start: u64, end: u64) -> Vec<Block> {
        let mut out = Vec::new();
        let mut at = start;
        while at < end && at < SPACE_END {
            let b = self.query(at);
            at = b.end;
            out.push(b);
            if b.end == 0 {
                break;
            }
        }
        out
    }

    /// Whether every block of `start..end` satisfies `ok`.
    pub(crate) fn all(&self, start: u64, end: u64, ok: impl Fn(&Block) -> bool) -> bool {
        end <= SPACE_END && self.blocks(start, end).iter().all(ok)
    }

    /// Gives each block of `start..end`, cut to the range, the properties that `f` makes of it.
    pub(crate) fn rewrite(&mut self, start: u64, end: u64, f: impl Fn(&Block) -> Block) {
        for b in self.blocks(start, end) {
            let n = f(&b);
            self.set(
                b.start.max(start),
                b.end.min(end),
                n.state,
                n.perm,
                n.attr,
                n.src,
            );
        }
    }

    fn split_at(&mut self, at: u64) {
        if at == 0 || at >= SPACE_END {
            return;
        }
        let Some((&s, &e)) = self.map.range(..=at).next_back() else {
            return;
        };
        if s == at || e.end <= at {
            return;
        }
        self.map.insert(s, Entry { end: at, ..e });
        self.map.insert(at, e);
    }

    /// Gives `start..end` the properties. The range is inside the space and not empty.
    pub(crate) fn set(&mut self, start: u64, end: u64, st: u32, perm: u32, attr: u32, src: u64) {
        if start >= end || end > SPACE_END {
            return;
        }
        self.split_at(start);
        self.split_at(end);
        let inside: Vec<u64> = self.map.range(start..end).map(|(k, _)| *k).collect();
        for k in inside {
            self.map.remove(&k);
        }
        self.map.insert(
            start,
            Entry {
                end,
                state: st,
                perm,
                attr,
                src,
            },
        );
        self.join(start);
    }

    fn joinable(a: &Entry, b: &Entry) -> bool {
        a.state == b.state && a.perm == b.perm && a.attr == b.attr && a.src == 0 && b.src == 0
    }

    /// Joins the block that starts at `at` with its equal neighbours.
    fn join(&mut self, at: u64) {
        let mut at = at;
        if let Some((&ps, &p)) = self.map.range(..at).next_back()
            && let Some(&cur) = self.map.get(&at)
            && p.end == at
            && Self::joinable(&p, &cur)
        {
            self.map.remove(&at);
            self.map.insert(ps, Entry { end: cur.end, ..p });
            at = ps;
        }
        if let Some(&cur) = self.map.get(&at)
            && let Some(&next) = self.map.get(&cur.end)
            && Self::joinable(&cur, &next)
        {
            self.map.remove(&cur.end);
            self.map.insert(
                at,
                Entry {
                    end: next.end,
                    ..cur
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_splits_and_the_equal_neighbours_join_again() {
        let mut m = MemoryMap::new();
        m.set(0x1000, 0x5000, state::NORMAL, perm::RW, 0, 0);
        assert_eq!(m.query(0x2000).start, 0x1000);
        assert_eq!(m.query(0x2000).end, 0x5000);
        m.set(0x2000, 0x3000, state::NORMAL, perm::NONE, attr::LOCKED, 0);
        assert_eq!(m.blocks(0x1000, 0x5000).len(), 3);
        m.set(0x2000, 0x3000, state::NORMAL, perm::RW, 0, 0);
        assert_eq!(m.blocks(0x1000, 0x5000).len(), 1);
        assert_eq!(m.query(0x0).state, state::INACCESSIBLE);
        assert_eq!(m.query(0x5000).state, state::INACCESSIBLE);
    }

    #[test]
    fn a_block_with_a_source_is_not_joined() {
        let mut m = MemoryMap::new();
        m.set(0x1000, 0x2000, state::STACK, perm::RW, 0, 0x9000);
        m.set(0x2000, 0x3000, state::STACK, perm::RW, 0, 0xA000);
        assert_eq!(m.blocks(0x1000, 0x3000).len(), 2);
        assert_eq!(m.query(0x1000).src, 0x9000);
    }

    #[test]
    fn the_space_past_the_end_is_one_block_to_the_top() {
        let m = MemoryMap::new();
        let b = m.query(SPACE_END + 5);
        assert_eq!(b.start, SPACE_END);
        assert_eq!(b.size(), 0u64.wrapping_sub(SPACE_END));
    }
}
