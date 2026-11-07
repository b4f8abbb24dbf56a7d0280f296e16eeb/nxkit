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
    /// A new texture is `GuestNewer`, because the guest fills the memory before the GPU copy exists.
    pub fn create(&mut self, desc: TextureDesc, address: GpuAddr) -> Result<TexId> {
        let layout = desc.layout()?;
        address.offset(layout.size)?;
        let id = TexId(
            u32::try_from(self.records.len())
                .map_err(|_| NvnError::BadPool("too many textures"))?,
        );
        self.records.push(TextureRecord {
            id,
            desc,
            layout,
            address,
            residency: Residency::GuestNewer,
            frame_local: false,
            alive: true,
        });
        self.by_address.insert((address.get(), id.0), id);
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
    /// This is the path for a CPU write into texture memory.
    pub fn guest_wrote(&mut self, start: GpuAddr, len: u64, hit: &mut Vec<TexId>) {
        let (lo, hi) = (start.get(), start.get().saturating_add(len));
        let from = lo.saturating_sub(self.largest);
        let first = hit.len();
        for (_, id) in self.by_address.range((from, 0)..(hi, 0)) {
            hit.push(*id);
        }
        let mut keep = first;
        for i in first..hit.len() {
            let r = &mut self.records[hit[i].0 as usize];
            if r.end() > lo {
                r.residency = Residency::GuestNewer;
                hit[keep] = hit[i];
                keep += 1;
            }
        }
        hit.truncate(keep);
    }

    /// Marks a texture as written by the GPU.
    pub fn gpu_wrote(&mut self, id: TexId) -> Result<()> {
        self.get_mut(id)?.residency = Residency::GpuNewer;
        Ok(())
    }

    /// Marks a texture as synchronised after the backend copied one side to the other.
    pub fn synced(&mut self, id: TexId) -> Result<()> {
        self.get_mut(id)?.residency = Residency::InSync;
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
    fn storage_must_fit_in_40_bits() {
        let mut t = ResourceTable::new();
        assert!(t.create(rgba(64, 64), at((1 << 40) - 0x100)).is_err());
        assert!(t.create(rgba(0, 64), at(0)).is_err());
    }
}
