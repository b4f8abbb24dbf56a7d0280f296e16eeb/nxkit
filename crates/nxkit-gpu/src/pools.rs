//! The map from a guest GPU address to the buffer of its NVN memory pool.
//!
//! The game names memory by guest GPU address. Every NVN memory pool gets its own no-copy buffer over the pool's pages
//! rounded out to 16 KiB. [`PoolSnapshot`] is the immutable map of one moment: the pools sorted by start, disjoint,
//! and the buffer of each. [`PoolSnapshot::resolve`] turns a guest address and a length into the buffer, the offset in
//! it and the real address the GPU uses. [`PoolSet`] owns the current snapshot and builds a new one for every change
//! (copy on write), so the encode threads read a snapshot without a lock.
//!
//! The logic is generic over the backing `B` (the Metal buffer in production, a fake in the tests). Nothing here
//! needs a GPU. `docs/plans/gpu-buffer-per-pool.md` has the design (B.2, B.4, B.6, B.9).
//!
//! Two kinds of range appear. A pool has a guest range `[start, end)` that the game gave. Its buffer covers the same
//! range rounded out to [`PAGE`]. Neighbouring pools that are 4 KiB aligned share a rounded page, so their buffers
//! overlap. Each entry keeps its own buffer, and a resolve never reaches past the entry's own `[start, end)`.
//!
//! The cost of one registration is a binary search, a copy of the entry list (an `Arc` clone and four numbers per
//! entry, about 4,500 entries) and the two pin calls and the buffer make. One resolve is a binary search over the
//! entries and no lock, no allocation.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use nxkit_metal::GuestMemory;

mod life;
mod set;
#[cfg(test)]
mod tests;

use crate::overlay::Overlay;
use life::{Dead, Pinner, Yard};
pub use set::{MetalPoolSet, PoolSet};

/// The page size that a no-copy buffer needs for its start and its length.
pub const PAGE: u64 = nxkit_metal::PAGE_SIZE;

/// What the map needs from a backing buffer.
pub trait Backing: Send + Sync + 'static {
    /// The real address the GPU uses for the first byte of the backing.
    fn real_base(&self) -> u64;
}

impl Backing for GuestMemory {
    fn real_base(&self) -> u64 {
        self.real_base()
    }
}

/// The kernel side of the pages under a buffer. The addresses are host addresses of the rounded range. The set calls
/// `pin` before it makes the buffer and `unpin` after the buffer is gone. Every `pin` has one `unpin` of the same
/// range, so an implementation counts.
pub trait PagePins: Send + Sync {
    /// The range `[start, end)` gets a buffer.
    fn pin(&self, start: u64, end: u64);
    /// The buffer over `[start, end)` is gone.
    fn unpin(&self, start: u64, end: u64);
}

/// Makes the backing of a rounded range. The addresses are host addresses; `len` is a multiple of [`PAGE`].
/// The error is the reason, for the log.
pub trait MakeBacking<B>: Send + Sync {
    /// Makes the backing, or says why not.
    fn make(&self, host_start: u64, len: u64) -> Result<B, String>;
}

impl<B, F> MakeBacking<B> for F
where
    F: Fn(u64, u64) -> Result<B, String> + Send + Sync,
{
    fn make(&self, host_start: u64, len: u64) -> Result<B, String> {
        self(host_start, len)
    }
}

/// Why a range did not resolve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Miss {
    /// The first byte lies in no pool.
    NoPool,
    /// The range starts in a pool and does not end in it (or `va + len` wraps). `pool_end` is the end of that pool.
    CrossesEdge {
        /// The end of the pool the range starts in.
        pool_end: u64,
    },
    /// The pool exists but has no buffer, because making it failed.
    NoBuffer,
}

/// Where a range lies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Loc {
    /// The index of the entry in [`PoolSnapshot::entries`].
    pub pool: u32,
    /// The offset of the first byte in the backing.
    pub offset: u64,
    /// The real address of the first byte.
    pub real: u64,
    /// The bytes from the first byte to the end of the pool. For a range in the mirror: to the end of its run.
    pub room: u64,
    /// The index of the mirror run in `Overlay::runs` when the range lies in the mirror (`NO_RUN` otherwise); then
    /// `offset` is the offset in the buffer of that run and `real` the address in it.
    pub run: u32,
}

/// [`Loc::run`] of a range in guest memory.
pub(crate) const NO_RUN: u32 = u32::MAX;

/// A range that cannot be a pool: empty, or it wraps, or it starts below the base of the address space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BadRange {
    /// The first guest address.
    pub start: u64,
    /// The length in bytes.
    pub len: u64,
}

/// What the graveyard dropped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Drained {
    /// The backings dropped.
    pub count: usize,
    /// Their rounded lengths together.
    pub bytes: u64,
}

/// The numbers for the memory report. Nothing here prints.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PoolStats {
    /// Buffers of the pools in the current snapshot.
    pub live: u64,
    /// The sum of their rounded lengths.
    pub live_bytes: u64,
    /// Buffers of pools that left the current snapshot and that a frame or a cache still holds.
    pub retired: u64,
    /// The sum of their rounded lengths.
    pub retired_bytes: u64,
    /// Buffers that wait in the graveyard.
    pub graveyard: u64,
    /// The sum of their rounded lengths.
    pub graveyard_bytes: u64,
    /// Pinned ranges (one per buffer that exists, in the graveyard or not).
    pub pins: u64,
    /// The bytes of the pinned ranges.
    pub pin_bytes: u64,
}

/// A backing with its place in the address space.
pub struct PoolBuffer<B: Backing> {
    mem: Option<B>,
    /// The guest address of the first byte of the backing.
    va_start: u64,
    /// The rounded length.
    len: u64,
    /// The host address of the first byte.
    host_start: u64,
    flags: u32,
    /// Absent for the whole mode: the backing then drops in place and has no pin.
    life: Option<Life<B>>,
}

struct Life<B: Backing> {
    yard: Weak<Yard<B>>,
    pinner: Arc<Pinner>,
}

impl<B: Backing> PoolBuffer<B> {
    /// The backing, or `None` when making it failed.
    pub fn mem(&self) -> Option<&B> {
        self.mem.as_ref()
    }

    /// The guest address of the first byte of the backing.
    pub fn va_start(&self) -> u64 {
        self.va_start
    }

    /// The rounded length in bytes.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Always false.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// The `NVNmemoryPoolFlags` of the game.
    pub fn flags(&self) -> u32 {
        self.flags
    }
}

impl<B: Backing> Drop for PoolBuffer<B> {
    /// The last owner may be a Metal completion handler, and a backing must not drop there (its drop touches the
    /// residency set). It goes to the graveyard and the encoder drops it. If the graveyard is gone, the backing drops
    /// here, and the pin goes with it.
    fn drop(&mut self) {
        let Some(mem) = self.mem.take() else { return };
        let Some(life) = self.life.take() else {
            return drop(mem);
        };
        life.pinner.alive.fetch_sub(1, Ordering::Relaxed);
        life.pinner
            .alive_bytes
            .fetch_sub(self.len, Ordering::Relaxed);
        let dead = Dead {
            mem,
            host_start: self.host_start,
            len: self.len,
        };
        match life.yard.upgrade() {
            Some(yard) => yard.push(dead),
            None => dead.release(&life.pinner),
        }
    }
}

/// One pool of a snapshot.
pub struct PoolEntry<B: Backing> {
    /// The first guest address of the range the game gave (not rounded).
    pub start: u64,
    /// The end of that range.
    pub end: u64,
    /// A new number for every registration.
    pub id: u64,
    /// The buffer, shared with every snapshot that holds this entry.
    pub buf: Arc<PoolBuffer<B>>,
}

impl<B: Backing> Clone for PoolEntry<B> {
    fn clone(&self) -> Self {
        Self {
            start: self.start,
            end: self.end,
            id: self.id,
            buf: Arc::clone(&self.buf),
        }
    }
}

/// The pools at one moment. Immutable.
pub struct PoolSnapshot<B: Backing = GuestMemory> {
    generation: u64,
    entries: Vec<PoolEntry<B>>,
    /// The mirror of this flush (`NXKIT_PRIVATE=1`): member pages resolve to it.
    overlay: Option<Arc<Overlay<B>>>,
}

impl<B: Backing> PoolSnapshot<B> {
    /// A snapshot without pools.
    pub fn empty() -> Self {
        Self {
            generation: 0,
            entries: Vec::new(),
            overlay: None,
        }
    }

    /// The whole mode: one entry over `[base, base + len)` and one backing. Every address in the range resolves to
    /// `real_base + (va - base)`. The backing drops with the last snapshot that holds it, in place.
    pub fn whole(base: u64, len: u64, mem: B) -> Self {
        let end = base.saturating_add(len);
        let buf = PoolBuffer {
            mem: Some(mem),
            va_start: base,
            len: end - base,
            host_start: base,
            flags: 0,
            life: None,
        };
        let entry = PoolEntry {
            start: base,
            end,
            id: 0,
            buf: Arc::new(buf),
        };
        Self {
            generation: 0,
            entries: vec![entry],
            overlay: None,
        }
    }

    /// The same pools with the mirror of one flush: a range whose pages are all members resolves to the mirror. The entries
    /// are cloned (an `Arc` and four numbers each), so this is for the whole mode, which has one.
    #[must_use]
    pub fn with_overlay(&self, overlay: Arc<Overlay<B>>) -> Self {
        Self {
            generation: self.generation,
            entries: self.entries.clone(),
            overlay: Some(overlay),
        }
    }

    /// The mirror of this snapshot, if it has one.
    pub fn overlay(&self) -> Option<&Arc<Overlay<B>>> {
        self.overlay.as_ref()
    }

    /// The number of changes that led to this snapshot.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The pools, sorted by start and disjoint.
    pub fn entries(&self) -> &[PoolEntry<B>] {
        &self.entries
    }

    /// The index of the pool that holds `va`.
    fn find(&self, va: u64) -> Option<usize> {
        let after = self.entries.partition_point(|e| e.start <= va);
        let i = after.checked_sub(1)?;
        (va < self.entries[i].end).then_some(i)
    }

    /// The pool of the first byte, in guest memory. The range is not checked.
    fn resolve_start_guest(&self, va: u64) -> Result<Loc, Miss> {
        let i = self.find(va).ok_or(Miss::NoPool)?;
        let e = &self.entries[i];
        let mem = e.buf.mem.as_ref().ok_or(Miss::NoBuffer)?;
        let offset = va - e.buf.va_start;
        Ok(Loc {
            pool: i as u32,
            offset,
            real: mem.real_base().wrapping_add(offset),
            room: e.end - va,
            run: NO_RUN,
        })
    }

    /// The pool of the first byte. The range is not checked. With a mirror, a member page gives the mirror: `real` is the
    /// address in the buffer of its run and `room` runs to the end of the run.
    pub fn resolve_start(&self, va: u64) -> Result<Loc, Miss> {
        let loc = self.resolve_start_guest(va)?;
        if let Some(o) = &self.overlay
            && let Some((run, real, room)) = o.locate(va)
        {
            return Ok(Loc {
                pool: loc.pool,
                offset: va - o.runs()[run].va_start,
                real,
                room: room.min(loc.room),
                run: run as u32,
            });
        }
        Ok(loc)
    }

    /// The pool of `[va, va + len)`. The whole range must lie in one pool. A `len` of 0 resolves when `va` lies in a
    /// pool. The end of a pool is not in it. With a mirror, a range whose pages are all members gives the mirror; a range
    /// that starts on a member page and reaches past the members is counted and gives guest memory.
    pub fn resolve(&self, va: u64, len: u64) -> Result<Loc, Miss> {
        self.resolve_in(va, len, 0)
    }

    fn resolve_in(&self, va: u64, len: u64, class: u8) -> Result<Loc, Miss> {
        let loc = self.resolve_start(va)?;
        if loc.run != NO_RUN {
            let whole = self
                .overlay
                .as_ref()
                .is_some_and(|o| len <= loc.room && o.all_members(va, len));
            if whole {
                return Ok(loc);
            }
            let split = if self
                .overlay
                .as_ref()
                .is_some_and(|o| o.all_members(va, len))
            {
                2
            } else {
                0
            };
            crate::overlay::note_partial(class, split, va, len);
            return self.resolve_guest(va, len);
        }
        if len > loc.room {
            let pool_end = self.entries[loc.pool as usize].end;
            return Err(Miss::CrossesEdge { pool_end });
        }
        if let Some(o) = &self.overlay
            && len > 1
            && o.any_member(va, len)
        {
            // Starts outside the members and reaches into them: guest memory is stale there.
            crate::overlay::note_partial(class, 1, va, len);
        }
        Ok(loc)
    }

    /// Counts a binding `[va, va + len)` that is partly on members and partly off them, for the sites that resolve only
    /// the start of a range and clip the length to the room ([`resolve_start_c`](Self::resolve_start_c)).
    pub(crate) fn check_span(&self, va: u64, len: u64, class: u8) {
        let Some(o) = &self.overlay else { return };
        if len <= 1 || o.off(class) {
            return;
        }
        let first = o.member(va);
        if first && !o.all_members(va, len) {
            crate::overlay::note_partial(class, 0, va, len);
        } else if !first && o.any_member(va, len) {
            crate::overlay::note_partial(class, 1, va, len);
        }
    }

    /// [`resolve_start`](Self::resolve_start) for a resolve class that a knob may bind to guest memory.
    pub(crate) fn resolve_start_c(&self, va: u64, class: u8) -> Result<Loc, Miss> {
        if self.overlay.as_ref().is_some_and(|o| o.off(class)) {
            return self.resolve_start_guest(va);
        }
        self.resolve_start(va)
    }

    /// [`resolve`](Self::resolve) for a resolve class that a knob may bind to guest memory.
    pub(crate) fn resolve_c(&self, va: u64, len: u64, class: u8) -> Result<Loc, Miss> {
        if self.overlay.as_ref().is_some_and(|o| o.off(class)) {
            return self.resolve_guest(va, len);
        }
        self.resolve_in(va, len, class)
    }

    /// Like [`resolve`](Self::resolve) but always in guest memory, for the table of raw global memory, which splits the
    /// pools at the member runs itself.
    pub fn resolve_guest(&self, va: u64, len: u64) -> Result<Loc, Miss> {
        let loc = self.resolve_start_guest(va)?;
        if len > loc.room {
            let pool_end = self.entries[loc.pool as usize].end;
            return Err(Miss::CrossesEdge { pool_end });
        }
        Ok(loc)
    }
}
