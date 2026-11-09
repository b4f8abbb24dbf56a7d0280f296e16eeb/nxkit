//! The life of the texture records, for the memory report.
//!
//! A texture record is made by `nvnTextureInitialize`. It is shared when a live record has the same address and
//! description. `nvnTextureFinalize` drops the record at once, or keeps it as an orphan while a texture pool slot still
//! names it; the orphan goes when its last slot is registered with another texture, or it is a normal live record again
//! when the game initializes the same texture once more. This module holds the front end methods of that life
//! ([`FrontEnd::create_texture_shared`], [`FrontEnd::finalize_texture`], [`FrontEnd::register_texture`]) and counts
//! what they do.
//!
//! The counters are relaxed atomics, one add per call, no lock and no allocation. Every [`FrontEnd`] also keeps its own
//! [`Counts`] ([`FrontEnd::texlife`]), which the tests read. The host reads the process wide record with [`snapshot`]
//! to print `nxkit-mem texlife`. Nothing in the front end or the backend looks at it.
//!
//! The live records and orphans whose memory lies in no live memory pool cannot be kept by events, because a pool goes
//! at any time. [`FrontEnd::end_frame`] walks the live records every [`SCAN_EVERY`] frames and stores the result
//! ([`FrontEnd::scan_outside_pools`]). A record is inside a pool when its start lies in the range of a pool of
//! [`crate::pools`]. When no pool is registered at all, every record is outside.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use crate::backend::PoolInfo;
use crate::error::Result;
use crate::front::FrontEnd;
use crate::layout::TextureDesc;
use crate::mem::GuestMem;
use crate::resource::{ResourceTable, TexId};
use crate::space::GpuSpace;
use crate::tic::TextureView;

/// The live records are checked against the live pools every this many frame ends.
pub const SCAN_EVERY: u64 = 60;

/// What the texture life did, since the start (the first six) and now (the last two).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// `nvnTextureInitialize` calls.
    pub inits: u64,
    /// Of these, the ones that shared a live record.
    pub shared: u64,
    /// Of the shared ones, the ones that took an orphan back.
    pub reshared: u64,
    /// Finalizes that dropped the record at once.
    pub dropped_now: u64,
    /// Finalizes that left an orphan.
    pub orphaned: u64,
    /// Orphans that went later, when their last slot got another texture.
    pub released: u64,
    /// The orphans now.
    pub orphans: u64,
    /// The bytes of the orphans now (`TextureLayout::size`).
    pub orphan_bytes: u64,
}

/// A count and the bytes of the things counted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Held {
    /// How many.
    pub count: u64,
    /// Their bytes (`TextureLayout::size`).
    pub bytes: u64,
}

/// The records that lie in no live memory pool, at the last scan.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Outside {
    /// The live records.
    pub records: Held,
    /// Of these, the orphans.
    pub orphans: Held,
}

/// The process wide record: the counts of all front ends, the table sizes and the last scan.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// The counts.
    pub counts: Counts,
    /// The records made since the start (ids are never used again).
    pub made: u64,
    /// The live records now.
    pub live: u64,
    /// The last scan against the live pools.
    pub outside: Outside,
}

static INITS: AtomicU64 = AtomicU64::new(0);
static SHARED: AtomicU64 = AtomicU64::new(0);
static RESHARED: AtomicU64 = AtomicU64::new(0);
static DROPPED_NOW: AtomicU64 = AtomicU64::new(0);
static ORPHANED: AtomicU64 = AtomicU64::new(0);
static RELEASED: AtomicU64 = AtomicU64::new(0);
static ORPHANS: AtomicU64 = AtomicU64::new(0);
static ORPHAN_BYTES: AtomicU64 = AtomicU64::new(0);
static MADE: AtomicU64 = AtomicU64::new(0);
static LIVE: AtomicU64 = AtomicU64::new(0);
static OUT: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

/// The process wide record now.
pub fn snapshot() -> Snapshot {
    Snapshot {
        counts: Counts {
            inits: INITS.load(Relaxed),
            shared: SHARED.load(Relaxed),
            reshared: RESHARED.load(Relaxed),
            dropped_now: DROPPED_NOW.load(Relaxed),
            orphaned: ORPHANED.load(Relaxed),
            released: RELEASED.load(Relaxed),
            orphans: ORPHANS.load(Relaxed),
            orphan_bytes: ORPHAN_BYTES.load(Relaxed),
        },
        made: MADE.load(Relaxed),
        live: LIVE.load(Relaxed),
        outside: Outside {
            records: Held {
                count: OUT[0].load(Relaxed),
                bytes: OUT[1].load(Relaxed),
            },
            orphans: Held {
                count: OUT[2].load(Relaxed),
                bytes: OUT[3].load(Relaxed),
            },
        },
    }
}

/// The orphans of one front end and its counts.
#[derive(Debug, Default)]
pub(crate) struct Life {
    /// The textures the game finalized while a pool slot still holds them, with their bytes.
    orphans: Vec<(TexId, u64)>,
    counts: Counts,
    /// The sizes of the table at the last change, to add the difference to the process wide record.
    table: (u64, u64),
}

impl Life {
    fn is_orphan(&self, id: TexId) -> bool {
        self.orphans.iter().any(|&(o, _)| o == id)
    }

    /// An initialize. `shared` when it named a live record.
    fn init(&mut self, id: TexId, shared: bool) {
        self.counts.inits += 1;
        INITS.fetch_add(1, Relaxed);
        if !shared {
            return;
        }
        self.counts.shared += 1;
        SHARED.fetch_add(1, Relaxed);
        // An orphan that is named again is a live record like any other.
        if let Some(i) = self.orphans.iter().position(|&(o, _)| o == id) {
            let (_, bytes) = self.orphans.swap_remove(i);
            self.counts.reshared += 1;
            RESHARED.fetch_add(1, Relaxed);
            self.orphan_out(bytes);
        }
    }

    /// A finalize left the record as an orphan. A second finalize of the same orphan counts nothing.
    fn orphaned(&mut self, id: TexId, bytes: u64) {
        if self.is_orphan(id) {
            return;
        }
        self.orphans.push((id, bytes));
        self.counts.orphaned += 1;
        self.counts.orphans += 1;
        self.counts.orphan_bytes += bytes;
        ORPHANED.fetch_add(1, Relaxed);
        ORPHANS.fetch_add(1, Relaxed);
        ORPHAN_BYTES.fetch_add(bytes, Relaxed);
    }

    /// The last slot of an orphan got another texture. False when `id` is not an orphan.
    fn released(&mut self, id: TexId) -> bool {
        let Some(i) = self.orphans.iter().position(|&(o, _)| o == id) else {
            return false;
        };
        let (_, bytes) = self.orphans.swap_remove(i);
        self.counts.released += 1;
        RELEASED.fetch_add(1, Relaxed);
        self.orphan_out(bytes);
        true
    }

    fn orphan_out(&mut self, bytes: u64) {
        self.counts.orphans -= 1;
        self.counts.orphan_bytes -= bytes;
        ORPHANS.fetch_sub(1, Relaxed);
        ORPHAN_BYTES.fetch_sub(bytes, Relaxed);
    }

    /// The table sizes after a change.
    fn table(&mut self, table: &ResourceTable) {
        let (made, live) = table.counts();
        let (made, live) = (made as u64, live as u64);
        MADE.fetch_add(made.wrapping_sub(self.table.0), Relaxed);
        LIVE.fetch_add(live.wrapping_sub(self.table.1), Relaxed);
        self.table = (made, live);
    }
}

/// True when the host range starting at `start` lies in none of `pools` (sorted by start, as [`crate::pools`] keeps them).
fn outside_pools(pools: &[PoolInfo], start: u64) -> bool {
    let k = pools.partition_point(|p| p.host <= start);
    k == 0 || start - pools[k - 1].host >= pools[k - 1].len
}

impl<S: GpuSpace, M: GuestMem> FrontEnd<S, M> {
    /// The counts of this front end.
    pub fn texlife(&self) -> Counts {
        self.life.counts
    }

    /// Like [`FrontEnd::create_texture`]. A live texture with the same address and description is shared and comes back
    /// with `true` (see [`ResourceTable::create_shared`]); the backend already knows it. An orphan that is shared
    /// is a live record again: it leaves the orphan set, so the register of another texture in its last slot does not
    /// drop it.
    pub fn create_texture_shared(
        &mut self,
        desc: TextureDesc,
        host_address: u64,
    ) -> Result<(TexId, bool)> {
        let size = desc.layout()?.size;
        let address = self.space.gpu_address(host_address, size)?;
        let (id, shared) = self.table.create_shared(desc, address)?;
        self.life.init(id, shared);
        self.life.table(&self.table);
        Ok((id, shared))
    }

    /// Destroys a texture. This is `nvnTextureFinalize`.
    ///
    /// On the hardware a texture pool slot is a copy of the texture header in memory, so it stays usable after the
    /// texture object is finalized: games keep sampling such slots. The record therefore lives on while a slot holds
    /// it and goes when the last slot is registered with another texture.
    /// The result is false while the record lives on; [`FrontEnd::take_dropped`] gives it when it goes.
    pub fn finalize_texture(&mut self, id: TexId) -> Result<bool> {
        // Another texture object still names the record.
        if !self.table.release_ref(id)? {
            return Ok(false);
        }
        if self.descriptors.has_slots(id) {
            let bytes = self.table.get(id)?.layout.size;
            self.life.orphaned(id, bytes);
            return Ok(false);
        }
        self.life.counts.dropped_now += 1;
        DROPPED_NOW.fetch_add(1, Relaxed);
        self.drop_texture(id)?;
        Ok(true)
    }

    /// The finalized textures whose last slot went since the previous call.
    pub fn take_dropped(&mut self) -> Vec<TexId> {
        std::mem::take(&mut self.dropped)
    }

    pub(crate) fn drop_texture(&mut self, id: TexId) -> Result<()> {
        self.table.finalize(id)?;
        self.life.table(&self.table);
        self.descriptors.drop_texture(id);
        self.queues.values_mut().for_each(|q| q.forget(id));
        Ok(())
    }

    /// Registers a texture. This is `nvnTexturePoolRegisterTexture`.
    /// A recording that bound the old texture of this id is decoded again at its next submit.
    pub fn register_texture(
        &mut self,
        id: u32,
        texture: TexId,
        view: TextureView,
    ) -> Result<crate::descriptors::TextureSlot> {
        let old = self.descriptors.texture(id).ok().map(|s| s.texture);
        let slot = self
            .descriptors
            .register_texture(&self.table, id, texture, view)?;
        if let Some(old) = old.filter(|&o| o != texture)
            && !self.descriptors.has_slots(old)
            && self.life.released(old)
        {
            self.drop_texture(old)?;
            self.dropped.push(old);
        }
        Ok(slot)
    }

    /// Checks the live records and the orphans against the live memory pools and stores the result in the process wide
    /// record. A record whose range is not in the address space is left out.
    pub fn scan_outside_pools(&self) -> Outside {
        let pools = crate::pools::snapshot().pools;
        let out = |bytes: u64, gpu: crate::space::GpuAddr| {
            self.space
                .host_address(gpu, bytes)
                .is_ok_and(|h| outside_pools(&pools, h))
        };
        let mut o = Outside::default();
        for r in self.table.live() {
            if out(r.layout.size, r.address) {
                o.records.count += 1;
                o.records.bytes += r.layout.size;
            }
        }
        for &(id, bytes) in &self.life.orphans {
            if let Ok(r) = self.table.get(id)
                && out(bytes, r.address)
            {
                o.orphans.count += 1;
                o.orphans.bytes += bytes;
            }
        }
        let new = [
            o.records.count,
            o.records.bytes,
            o.orphans.count,
            o.orphans.bytes,
        ];
        OUT.iter().zip(new).for_each(|(a, v)| a.store(v, Relaxed));
        o
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(host: u64, len: u64) -> PoolInfo {
        PoolInfo {
            host,
            len,
            flags: 0,
        }
    }

    #[test]
    fn a_start_is_inside_a_pool_when_it_is_in_its_range() {
        let pools = [pool(0x1000, 0x1000), pool(0x4000, 0x1000)];
        assert!(!outside_pools(&pools, 0x1000));
        assert!(!outside_pools(&pools, 0x1fff));
        assert!(outside_pools(&pools, 0x2000));
        assert!(outside_pools(&pools, 0xfff));
        assert!(!outside_pools(&pools, 0x4800));
        assert!(outside_pools(&pools, 0x5000));
        assert!(outside_pools(&[], 0x1000));
    }
}
