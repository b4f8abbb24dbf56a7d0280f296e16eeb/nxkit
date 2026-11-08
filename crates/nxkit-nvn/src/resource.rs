//! The resource table: every texture of the game, found by its GPU address.
//!
//! The table gives the backend the way from a descriptor to the Metal texture. A [`TexId`] is the key that the backend
//! uses for its own table. The table also tracks which side holds the current data, the guest memory or the GPU copy,
//! so a CPU write into texture memory is found later.

use std::collections::BTreeMap;

use crate::error::{NvnError, Result};
use crate::layout::{TextureDesc, TextureLayout};
use crate::space::GpuAddr;

/// The key of a texture in the resource table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TexId(pub u32);

/// Which copy of a texture holds the current data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Residency {
    /// The guest memory and the GPU copy hold the same data.
    InSync,
    /// The guest wrote the memory. The GPU copy is old.
    GuestNewer,
    /// The GPU wrote the texture. The guest memory is old.
    GpuNewer,
}

/// One texture.
#[derive(Clone, Copy, Debug)]
pub struct TextureRecord {
    /// The key of the texture.
    pub id: TexId,
    /// What the game asked for.
    pub desc: TextureDesc,
    /// The storage layout.
    pub layout: TextureLayout,
    /// The GPU address of the first byte.
    pub address: GpuAddr,
    /// Which copy is current.
    pub residency: Residency,
    /// True when the host knows that the contents are dead at the end of each frame.
    pub frame_local: bool,
    /// Counts the CPU writes that hit this texture. A backend that encodes from a copy of the record compares it
    /// before it applies what the GPU did ([`ResourceTable::gpu_wrote_at`]).
    pub write_epoch: u32,
    alive: bool,
}

impl TextureRecord {
    /// The GPU address after the last byte of the texture.
    pub fn end(&self) -> u64 {
        self.address.get() + self.layout.size
    }
}

/// The textures of the game.
#[derive(Debug, Default)]
pub struct ResourceTable {
    records: Vec<TextureRecord>,
    by_address: BTreeMap<(u64, u32), TexId>,
    largest: u64,
    /// The residency that every live texture with this address and description has, while that is known. A game
    /// that initializes texture objects again and again over the same storage piles up exact aliases, and setting
    /// the residency of each written texture walked all of them every frame (in Breath of the Wild as long as the
    /// whole encode, and growing with the play time). Every other change of a residency drops the entry.
    group_residency: std::collections::HashMap<(u64, crate::layout::TextureDesc), Residency>,
}

impl ResourceTable {
    /// An empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of ids that were given out, including finalised textures.
    pub fn id_count(&self) -> usize {
        self.records.len()
    }

    /// Adds a texture at `address`. The storage must end below 2^40.
    /// New storage is `GuestNewer`. An exact alias inherits the residency of the existing storage.
    pub fn create(&mut self, desc: TextureDesc, address: GpuAddr) -> Result<TexId> {
        let layout = desc.layout()?;
        address.offset(layout.size)?;
        // Initializing another object over existing storage does not write that storage. Exact aliases inherit
        // its current owner, so a new wrapper does not upload stale guest bytes over GPU-written contents.
        let a = address.get();
        let residency = self
            .by_address
            .range((a, 0)..=(a, u32::MAX))
            .rev()
            .filter_map(|(_, id)| self.get(*id).ok())
            .find(|r| r.desc == desc)
            .map_or(Residency::GuestNewer, |r| r.residency);
        let id = TexId(
            u32::try_from(self.records.len())
                .map_err(|_| NvnError::BadPool("too many textures"))?,
        );
        self.records.push(TextureRecord {
            id,
            desc,
            layout,
            address,
            residency,
            frame_local: false,
            write_epoch: 0,
            alive: true,
        });
        self.by_address.insert((address.get(), id.0), id);
        self.group_residency.remove(&(address.get(), desc));
        self.largest = self.largest.max(layout.size);
        Ok(id)
    }

    /// Removes a texture. Its id is not used again.
    pub fn finalize(&mut self, id: TexId) -> Result<()> {
        let r = self.get_mut(id)?;
        r.alive = false;
        let key = (r.address.get(), id.0);
        self.by_address.remove(&key);
        Ok(())
    }

    /// The record of a live texture.
    pub fn get(&self, id: TexId) -> Result<&TextureRecord> {
        match self.records.get(id.0 as usize) {
            Some(r) if r.alive => Ok(r),
            _ => Err(NvnError::UnknownTexture(id.0)),
        }
    }

    fn get_mut(&mut self, id: TexId) -> Result<&mut TextureRecord> {
        match self.records.get_mut(id.0 as usize) {
            Some(r) if r.alive => Ok(r),
            _ => Err(NvnError::UnknownTexture(id.0)),
        }
    }

    /// The textures that start exactly at `address`, in creation order.
    pub fn at_address(&self, address: GpuAddr) -> impl Iterator<Item = TexId> + '_ {
        let a = address.get();
        self.by_address
            .range((a, 0)..=(a, u32::MAX))
            .map(|(_, id)| *id)
    }

    /// Marks every texture that overlaps `[start, start + len)` as `GuestNewer` and pushes its id into `hit`.
    /// This is the path for a CPU write into texture memory. A texture that the GPU wrote last is left alone.
    pub fn guest_wrote(&mut self, start: GpuAddr, len: u64, hit: &mut Vec<TexId>) {
        self.cpu_wrote(start, len, false, hit);
    }

    /// Like [`ResourceTable::guest_wrote`] for a write that is known to change texels (`nvnTextureWriteTexels`). It
    /// marks a texture that the GPU wrote last too.
    pub fn texels_written(&mut self, start: GpuAddr, len: u64, hit: &mut Vec<TexId>) {
        self.cpu_wrote(start, len, true, hit);
    }

    fn cpu_wrote(&mut self, start: GpuAddr, len: u64, texels: bool, hit: &mut Vec<TexId>) {
        let (lo, hi) = (start.get(), start.get().saturating_add(len));
        let from = lo.saturating_sub(self.largest);
        let first = hit.len();
        for (_, id) in self.by_address.range((from, 0)..(hi, 0)) {
            hit.push(*id);
        }
        let mut keep = first;
        for i in first..hit.len() {
            let r = &mut self.records[hit[i].0 as usize];
            // A texture that the GPU wrote keeps its GPU content: games flush whole pools after writing other
            // data into them, and taking the flush as new texels uploads guest bytes that never saw the GPU's
            // writes over them (BotW's terrain height and material arrays, rendered on the GPU each frame,
            // came out flat). nxnative treats flushes the same way, except for games whose profile asks.
            if r.end() > lo && (texels || r.residency != Residency::GpuNewer) {
                self.group_residency.remove(&(r.address.get(), r.desc));
                r.residency = Residency::GuestNewer;
                r.write_epoch = r.write_epoch.wrapping_add(1);
                hit[keep] = hit[i];
                keep += 1;
            }
        }
        hit.truncate(keep);
    }

    /// Marks a texture as written by the GPU.
    pub fn gpu_wrote(&mut self, id: TexId) -> Result<()> {
        self.set_alias_residency(id, Residency::GpuNewer, None)
    }

    /// Like [`ResourceTable::gpu_wrote`] for a frame that was encoded from a copy of the record. `epoch` is the
    /// [`TextureRecord::write_epoch`] of that copy. A CPU write that came after the copy keeps the texture `GuestNewer`.
    pub fn gpu_wrote_at(&mut self, id: TexId, epoch: u32) -> Result<()> {
        self.set_alias_residency(id, Residency::GpuNewer, Some(epoch))
    }

    /// Marks a texture as synchronised after the backend copied one side to the other.
    pub fn synced(&mut self, id: TexId) -> Result<()> {
        self.set_alias_residency(id, Residency::InSync, None)
    }

    /// Like [`ResourceTable::synced`] for a frame that was encoded from a copy of the record. See
    /// [`ResourceTable::gpu_wrote_at`].
    pub fn synced_at(&mut self, id: TexId, epoch: u32) -> Result<()> {
        self.set_alias_residency(id, Residency::InSync, Some(epoch))
    }

    /// Sets the residency of a texture and of its exact aliases. With `epoch` set, nothing changes when a CPU write hit
    /// the texture since the record had that epoch.
    fn set_alias_residency(&mut self, id: TexId, residency: Residency, epoch: Option<u32>) -> Result<()> {
        let record = self.get(id)?;
        if epoch.is_some_and(|e| e != record.write_epoch) {
            return Ok(());
        }
        let (addr, desc) = (record.address.get(), record.desc);
        if self.group_residency.get(&(addr, desc)) == Some(&residency) {
            return Ok(());
        }
        self.group_residency.insert((addr, desc), residency);
        for (_, alias) in self.by_address.range((addr, 0)..=(addr, u32::MAX)) {
            let r = &mut self.records[alias.0 as usize];
            if r.residency != residency && r.desc == desc {
                r.residency = residency;
            }
        }
        Ok(())
    }

    /// Tells that the contents of a texture are dead at the end of every frame.
    pub fn set_frame_local(&mut self, id: TexId, frame_local: bool) -> Result<()> {
        self.get_mut(id)?.frame_local = frame_local;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Target;

    fn rgba(w: u32, h: u32) -> TextureDesc {
        TextureDesc {
            target: Target::D2,
            format: 0x25,
            flags: 0,
            samples: 0,
            width: w,
            height: h,
            depth: 1,
            levels: 1,
            stride: 0,
            packaged_block: None,
        }
    }

    fn at(a: u64) -> GpuAddr {
        GpuAddr::new(a).unwrap()
    }

    #[test]
    fn finds_textures_by_address_in_creation_order() {
        let mut t = ResourceTable::new();
        let a = t.create(rgba(64, 64), at(0x1000)).unwrap();
        let b = t.create(rgba(64, 64), at(0x1000)).unwrap();
        let c = t.create(rgba(64, 64), at(0x90000)).unwrap();
        assert_eq!(t.at_address(at(0x1000)).collect::<Vec<_>>(), [a, b]);
        assert_eq!(t.at_address(at(0x90000)).collect::<Vec<_>>(), [c]);
        assert_eq!(t.at_address(at(0x2000)).count(), 0);
        t.finalize(a).unwrap();
        assert_eq!(t.at_address(at(0x1000)).collect::<Vec<_>>(), [b]);
        assert_eq!(t.get(a).unwrap_err(), NvnError::UnknownTexture(0));
    }

    #[test]
    fn a_cpu_write_marks_only_the_overlapping_textures() {
        let mut t = ResourceTable::new();
        let a = t.create(rgba(64, 64), at(0x10_0000)).unwrap();
        let b = t.create(rgba(64, 64), at(0x20_0000)).unwrap();
        let big = t.create(rgba(256, 256), at(0x30_0000)).unwrap();
        for id in [a, b, big] {
            t.synced(id).unwrap();
        }
        let mut hit = Vec::new();
        // a write into the middle of the big texture, which starts far before the write
        t.guest_wrote(at(0x30_0000 + 0x2_0000), 16, &mut hit);
        assert_eq!(hit, [big]);
        assert_eq!(t.get(big).unwrap().residency, Residency::GuestNewer);
        assert_eq!(t.get(a).unwrap().residency, Residency::InSync);
        hit.clear();
        // a write right after the end of `a` touches nothing
        let end = t.get(a).unwrap().end();
        t.guest_wrote(at(end), 4, &mut hit);
        assert!(hit.is_empty());
        t.guest_wrote(at(end - 1), 4, &mut hit);
        assert_eq!(hit, [a]);
    }

    #[test]
    fn gpu_writes_and_syncs_change_the_flag() {
        let mut t = ResourceTable::new();
        let a = t.create(rgba(8, 8), at(0x4000)).unwrap();
        assert_eq!(t.get(a).unwrap().residency, Residency::GuestNewer);
        t.synced(a).unwrap();
        t.gpu_wrote(a).unwrap();
        assert_eq!(t.get(a).unwrap().residency, Residency::GpuNewer);
        assert!(t.gpu_wrote(TexId(9)).is_err());
    }

    #[test]
    fn exact_aliases_share_residency_and_new_objects_preserve_gpu_contents() {
        let mut t = ResourceTable::new();
        let a = t.create(rgba(8, 8), at(0x4000)).unwrap();
        t.gpu_wrote(a).unwrap();
        let b = t.create(rgba(8, 8), at(0x4000)).unwrap();
        assert_eq!(t.get(b).unwrap().residency, Residency::GpuNewer);
        let c = t.create(rgba(16, 8), at(0x4000)).unwrap();
        assert_eq!(t.get(c).unwrap().residency, Residency::GuestNewer);
        t.synced(b).unwrap();
        assert_eq!(t.get(a).unwrap().residency, Residency::InSync);
        t.gpu_wrote(b).unwrap();
        assert_eq!(t.get(a).unwrap().residency, Residency::GpuNewer);
        let mut hit = Vec::new();
        t.guest_wrote(at(0x4000), 4, &mut hit);
        assert_eq!(t.get(a).unwrap().residency, Residency::GpuNewer);
        assert_eq!(t.get(b).unwrap().residency, Residency::GpuNewer);
        assert!(!hit.contains(&a) && !hit.contains(&b));
    }

    #[test]
    fn a_flush_marks_only_textures_the_gpu_did_not_write() {
        let mut t = ResourceTable::new();
        let gpu = t.create(rgba(8, 8), at(0x4000)).unwrap();
        let cpu = t.create(rgba(8, 8), at(0x8000)).unwrap();
        t.gpu_wrote(gpu).unwrap();
        t.synced(cpu).unwrap();
        let mut hit = Vec::new();
        t.guest_wrote(at(0x4000), 0x8000, &mut hit);
        assert_eq!(t.get(gpu).unwrap().residency, Residency::GpuNewer);
        assert_eq!(t.get(cpu).unwrap().residency, Residency::GuestNewer);
        assert_eq!(hit, vec![cpu]);
    }

    #[test]
    fn a_cpu_write_after_the_copy_wins_over_the_report_of_the_gpu() {
        let mut t = ResourceTable::new();
        let a = t.create(rgba(64, 64), at(0x1000)).unwrap();
        let seen = t.get(a).unwrap().write_epoch;
        t.guest_wrote(at(0x1000), 4, &mut Vec::new());
        t.gpu_wrote_at(a, seen).unwrap();
        t.synced_at(a, seen).unwrap();
        assert_eq!(t.get(a).unwrap().residency, Residency::GuestNewer);
        let now = t.get(a).unwrap().write_epoch;
        t.gpu_wrote_at(a, now).unwrap();
        assert_eq!(t.get(a).unwrap().residency, Residency::GpuNewer);
        t.synced_at(a, now).unwrap();
        assert_eq!(t.get(a).unwrap().residency, Residency::InSync);
    }

    #[test]
    fn a_copy_back_leaves_the_texture_in_sync_until_the_cpu_writes_it() {
        let mut t = ResourceTable::new();
        let a = t.create(rgba(64, 64), at(0x1000)).unwrap();
        let alias = t.create(rgba(64, 64), at(0x1000)).unwrap();
        // The frame renders into the texture and copies it back: the report applies the write, then the sync.
        let seen = t.get(a).unwrap().write_epoch;
        t.gpu_wrote_at(a, seen).unwrap();
        t.synced_at(a, seen).unwrap();
        assert_eq!(t.get(a).unwrap().residency, Residency::InSync);
        assert_eq!(t.get(alias).unwrap().residency, Residency::InSync);
        // A later flush or write of the memory is new data again, and the next use uploads it.
        let mut hit = Vec::new();
        t.guest_wrote(at(0x1000 + 0x100), 4, &mut hit);
        assert_eq!(hit, [a, alias]);
        assert_eq!(t.get(a).unwrap().residency, Residency::GuestNewer);
        // The next frame renders it again without a copy-back: the GPU copy is newer, and a flush does not undo it.
        let seen = t.get(a).unwrap().write_epoch;
        t.gpu_wrote_at(a, seen).unwrap();
        t.guest_wrote(at(0x1000), 4, &mut Vec::new());
        assert_eq!(t.get(a).unwrap().residency, Residency::GpuNewer);
        // `nvnTextureWriteTexels` always wins.
        t.texels_written(at(0x1000), 4, &mut Vec::new());
        assert_eq!(t.get(a).unwrap().residency, Residency::GuestNewer);
    }

    #[test]
    fn a_cpu_write_between_the_encode_and_the_copy_back_report_wins() {
        let mut t = ResourceTable::new();
        let a = t.create(rgba(64, 64), at(0x1000)).unwrap();
        t.synced(a).unwrap();
        // The encoder works from a copy of the record. The CPU writes the texture before the report comes back.
        let seen = t.get(a).unwrap().write_epoch;
        t.guest_wrote(at(0x1000), 4, &mut Vec::new());
        t.gpu_wrote_at(a, seen).unwrap();
        t.synced_at(a, seen).unwrap();
        assert_eq!(t.get(a).unwrap().residency, Residency::GuestNewer);
    }

    #[test]
    fn storage_must_fit_in_40_bits() {
        let mut t = ResourceTable::new();
        assert!(t.create(rgba(64, 64), at((1 << 40) - 0x100)).is_err());
        assert!(t.create(rgba(0, 64), at(0)).is_err());
    }

    #[test]
    fn exact_aliases_share_the_residency_after_a_cpu_write_too() {
        let mut t = ResourceTable::new();
        let a = t.create(rgba(16, 16), at(0x10_0000)).unwrap();
        let b = t.create(rgba(16, 16), at(0x10_0000)).unwrap();
        t.gpu_wrote(a).unwrap();
        assert_eq!(t.get(b).unwrap().residency, Residency::GpuNewer);
        let mut hit = Vec::new();
        t.texels_written(at(0x10_0000), 16, &mut hit);
        assert_eq!(t.get(a).unwrap().residency, Residency::GuestNewer);
        t.gpu_wrote(b).unwrap();
        assert_eq!(t.get(a).unwrap().residency, Residency::GpuNewer);
        let c = t.create(rgba(16, 16), at(0x10_0000)).unwrap();
        assert_eq!(t.get(c).unwrap().residency, Residency::GpuNewer);
        t.synced(a).unwrap();
        assert_eq!(t.get(c).unwrap().residency, Residency::InSync);
    }
}
