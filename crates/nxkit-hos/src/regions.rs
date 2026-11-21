//! The address regions that the SDK makes in the alias region, and the room the alias region has for one more.
//!
//! The SDK's `VammManager` hands the alias region out as address regions, all in guest code and without a svc. The
//! rewriter's observation gates in the `sdk` show each one (R46): the size at the entry of `AllocateAddressRegion`,
//! the address at its store through the out pointer, the address at `FreeAddressRegion`. The kernel keeps them in a
//! table, so that it can say how large a region the alias region still has room for (`Kernel::alias_room` in
//! `regionsvc`): the engines that ask for a region made for the Switch's 64 GiB alias region get that instead.

use std::collections::HashMap;
use std::sync::Mutex;
use std::thread::ThreadId;

/// What the regions made later keep of the alias region when [`Kernel::alias_room`] answers.
pub const ROOM_FOR_LATER: u64 = 0x1000_0000;
/// The SDK's node area at the start of the alias region with the SDK-wide cut of the build (1 GiB → 62 MiB). It counts
/// as used before the gate of its region is seen.
const NODE_AREA: u64 = 0x3e0_0000;
/// The step of the room.
const ROOM_STEP: u64 = 0x200_0000;
/// A region of this size or more is logged when it is made.
pub(crate) const LOGGED: u64 = 0x1000_0000;

/// The table of the regions and the sizes asked for by the threads in `AllocateAddressRegion`.
#[derive(Default)]
pub(crate) struct Regions {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// The regions by their offset in the alias region: the size.
    live: std::collections::BTreeMap<u64, u64>,
    /// The size each thread asked for, until its region comes back.
    asked: HashMap<ThreadId, u64>,
}

impl Regions {
    /// The calling thread asks for a region of `size` bytes.
    pub(crate) fn ask(&self, size: u64) {
        let mut g = crate::sync::lock(&self.inner);
        g.asked.insert(std::thread::current().id(), size);
    }

    /// The region of the calling thread's last ask is at `off` in the alias region (`None`: outside it). Returns the
    /// size it asked for.
    pub(crate) fn got(&self, off: Option<u64>) -> u64 {
        let mut g = crate::sync::lock(&self.inner);
        let size = g.asked.remove(&std::thread::current().id()).unwrap_or(0);
        if let Some(off) = off {
            g.live.insert(off, size);
        }
        size
    }

    /// The region at `off` is freed.
    pub(crate) fn free(&self, off: u64) {
        crate::sync::lock(&self.inner).live.remove(&off);
    }

    /// The room in an alias region of `alias_size` bytes (see [`room`]).
    pub(crate) fn room(&self, alias_size: u64) -> u64 {
        let g = crate::sync::lock(&self.inner);
        room(alias_size, g.live.iter().map(|(o, s)| (*o, *s)))
    }
}

/// What the alias region has room for in one more region: the end of the regions made so far (at least the node
/// area), less [`ROOM_FOR_LATER`], in 32 MiB steps. 0 when there is none.
pub(crate) fn room(alias_size: u64, live: impl Iterator<Item = (u64, u64)>) -> u64 {
    let reach = live
        .map(|(off, size)| off.saturating_add(size))
        .fold(NODE_AREA, u64::max);
    alias_size
        .checked_sub(reach)
        .and_then(|r| r.checked_sub(ROOM_FOR_LATER))
        .map_or(0, |r| r & !(ROOM_STEP - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_region_counts_until_it_is_freed() {
        let r = Regions::default();
        let alias = 0xC800_0000;
        let empty = r.room(alias);
        r.ask(0x4000_0000);
        assert_eq!(r.got(Some(NODE_AREA)), 0x4000_0000);
        assert_eq!(r.room(alias), empty - 0x4000_0000);
        r.ask(0x1000);
        assert_eq!(
            r.got(None),
            0x1000,
            "a region outside the alias region is not kept"
        );
        r.free(NODE_AREA);
        assert_eq!(r.room(alias), empty);
    }

    #[test]
    fn the_room_starts_behind_the_node_area_and_keeps_room_for_later_regions() {
        let alias = 0xC800_0000;
        assert_eq!(
            room(alias, std::iter::empty()),
            (alias - NODE_AREA - ROOM_FOR_LATER) & !(ROOM_STEP - 1)
        );
        // a region behind the node area moves the start
        let r = room(alias, [(NODE_AREA, 0x4000_0000)].into_iter());
        assert_eq!(
            r,
            (alias - NODE_AREA - 0x4000_0000 - ROOM_FOR_LATER) & !(ROOM_STEP - 1)
        );
        assert_eq!(r % ROOM_STEP, 0);
    }

    #[test]
    fn an_alias_region_without_room_gives_none() {
        assert_eq!(room(0x1000_0000, std::iter::empty()), 0);
        assert_eq!(room(0x1000_0000, [(0, 0x2000_0000)].into_iter()), 0);
    }
}
