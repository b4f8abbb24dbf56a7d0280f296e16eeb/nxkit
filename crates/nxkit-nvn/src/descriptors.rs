//! The texture pool and the sampler pool.
//!
//! The game registers a texture in a slot of a texture pool and gets a handle: the texture id in the low 20 bits and
//! the sampler id above them (`nvnDeviceGetTextureHandle`). The ids of all pools share one index space, as in nxnative.
//! A draw names textures by handle, so the front end resolves a handle to a texture of the resource table.

use crate::error::{NvnError, Result};
use crate::resource::{ResourceTable, TexId};
use crate::space::GpuAddr;
use crate::tic::{HEADER_BYTES, TextureView, Tic, Tsc};

/// The largest texture id that a handle can hold.
pub const MAX_TEXTURE_ID: u32 = (1 << 20) - 1;
/// The largest sampler id that a handle can hold.
pub const MAX_SAMPLER_ID: u32 = (1 << 12) - 1;

/// The id range of one pool and the memory that holds its headers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolRange {
    /// The first id of the pool.
    pub base: u32,
    /// The number of slots.
    pub count: u32,
    /// The GPU address of the header memory.
    pub memory: GpuAddr,
}

impl PoolRange {
    fn contains(&self, id: u32) -> bool {
        id >= self.base && id - self.base < self.count
    }

    /// The GPU address of the header of slot `id`.
    pub fn header_address(&self, id: u32) -> Result<GpuAddr> {
        self.memory
            .offset(u64::from(id - self.base) * HEADER_BYTES as u64)
    }
}

/// What a texture slot holds.
#[derive(Clone, Copy, Debug)]
pub struct TextureSlot {
    /// The texture in the resource table.
    pub texture: TexId,
    /// The header that the front end wrote for it.
    pub header: Tic,
}

/// The pools and their slots.
#[derive(Debug, Default)]
pub struct Descriptors {
    texture_pools: Vec<PoolRange>,
    sampler_pools: Vec<PoolRange>,
    textures: Vec<Option<TextureSlot>>,
    /// The slots that were registered with a texture, by texture id. An entry can be stale when a slot was
    /// registered again with another texture, so a reader checks the slot.
    slots_of: Vec<Vec<u32>>,
    samplers: Vec<Option<Tsc>>,
    /// Counts the changes that can make a handle name another texture ([`Descriptors::epoch`]).
    epoch: u64,
}

fn add_pool(pools: &mut Vec<PoolRange>, new: PoolRange, max_id: u32) -> Result<()> {
    if new.count == 0 {
        return Err(NvnError::BadPool("a pool has no slots"));
    }
    if new
        .base
        .checked_add(new.count)
        .is_none_or(|end| end - 1 > max_id)
    {
        return Err(NvnError::BadPool("ids above the handle limit"));
    }
    if pools
        .iter()
        .any(|p| new.base < p.base + p.count && p.base < new.base + new.count)
    {
        return Err(NvnError::BadPool("ids overlap another pool"));
    }
    pools.push(new);
    Ok(())
}

impl Descriptors {
    /// Empty pools.
    pub fn new() -> Self {
        Self::default()
    }

    /// A counter that changes whenever [`Descriptors::resolve`] could name another texture: when a slot gets a different
    /// texture, or loses it. Registering the texture a slot already holds, or a sampler, does not change it.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Adds a texture pool of `count` slots from id `base`, with its header memory at `memory`.
    pub fn add_texture_pool(&mut self, base: u32, count: u32, memory: GpuAddr) -> Result<()> {
        add_pool(
            &mut self.texture_pools,
            PoolRange {
                base,
                count,
                memory,
            },
            MAX_TEXTURE_ID,
        )?;
        let end = (base + count) as usize;
        if self.textures.len() < end {
            self.textures.resize(end, None);
        }
        Ok(())
    }

    /// Adds a sampler pool.
    pub fn add_sampler_pool(&mut self, base: u32, count: u32, memory: GpuAddr) -> Result<()> {
        add_pool(
            &mut self.sampler_pools,
            PoolRange {
                base,
                count,
                memory,
            },
            MAX_SAMPLER_ID,
        )?;
        let end = (base + count) as usize;
        if self.samplers.len() < end {
            self.samplers.resize(end, None);
        }
        Ok(())
    }

    /// The pool that holds a texture id.
    pub fn texture_pool_of(&self, id: u32) -> Option<&PoolRange> {
        self.texture_pools.iter().find(|p| p.contains(id))
    }

    /// Registers a texture in slot `id`. This is `nvnTexturePoolRegisterTexture`.
    /// The header is built from the texture of the resource table and the view.
    pub fn register_texture(
        &mut self,
        table: &ResourceTable,
        id: u32,
        texture: TexId,
        view: TextureView,
    ) -> Result<TextureSlot> {
        if self.texture_pool_of(id).is_none() {
            return Err(NvnError::UnknownDescriptor(id));
        }
        let r = table.get(texture)?;
        let header = Tic::for_texture(&r.desc, r.address, view)?;
        let slot = TextureSlot { texture, header };
        let old = self.textures[id as usize].replace(slot);
        if old.is_none_or(|o| o.texture != texture) {
            self.epoch += 1;
        }
        if let Some(old) = old.filter(|o| o.texture != texture)
            && let Some(slots) = self.slots_of.get_mut(old.texture.0 as usize)
        {
            slots.retain(|&s| s != id);
        }
        let index = texture.0 as usize;
        if self.slots_of.len() <= index {
            self.slots_of.resize_with(index + 1, Vec::new);
        }
        let slots = &mut self.slots_of[index];
        if !slots.contains(&id) {
            slots.push(id);
        }
        Ok(slot)
    }

    /// Registers a sampler in slot `id`. This is `nvnSamplerPoolRegisterSampler`.
    pub fn register_sampler(&mut self, id: u32, sampler: Tsc) -> Result<()> {
        if !self.sampler_pools.iter().any(|p| p.contains(id)) {
            return Err(NvnError::UnknownDescriptor(id));
        }
        self.samplers[id as usize] = Some(sampler);
        Ok(())
    }

    /// The texture slot `id`.
    pub fn texture(&self, id: u32) -> Result<&TextureSlot> {
        match self.textures.get(id as usize) {
            Some(Some(slot)) => Ok(slot),
            _ => Err(NvnError::UnknownDescriptor(id)),
        }
    }

    /// The sampler slot `id`, or None when no sampler is registered there.
    pub fn sampler(&self, id: u32) -> Option<&Tsc> {
        self.samplers.get(id as usize).and_then(Option::as_ref)
    }

    /// The handle for a texture id and a sampler id. This is `nvnDeviceGetTextureHandle`.
    pub fn handle(texture_id: u32, sampler_id: u32) -> u64 {
        u64::from(texture_id) | u64::from(sampler_id) << 20
    }

    /// The texture of a handle, and its sampler when one is registered.
    pub fn resolve(&self, handle: u64) -> Result<(&TextureSlot, Option<&Tsc>)> {
        let tex = (handle & u64::from(MAX_TEXTURE_ID)) as u32;
        let samp = ((handle >> 20) & u64::from(MAX_SAMPLER_ID)) as u32;
        Ok((self.texture(tex)?, self.sampler(samp)))
    }

    /// True while a pool slot holds `texture`.
    pub fn has_slots(&self, texture: TexId) -> bool {
        self.slots_of
            .get(texture.0 as usize)
            .is_some_and(|s| !s.is_empty())
    }

    /// Forgets every slot that holds `texture`. The game must not use them after `nvnTextureFinalize`.
    pub fn drop_texture(&mut self, texture: TexId) {
        let Some(slots) = self.slots_of.get_mut(texture.0 as usize) else {
            return;
        };
        for id in slots.drain(..) {
            let s = &mut self.textures[id as usize];
            if s.as_ref().is_some_and(|slot| slot.texture == texture) {
                *s = None;
                self.epoch += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Target, TextureDesc};

    fn table_with_one() -> (ResourceTable, TexId) {
        let mut t = ResourceTable::new();
        let d = TextureDesc {
            target: Target::D2,
            format: 0x25,
            flags: 0,
            samples: 0,
            width: 32,
            height: 32,
            depth: 1,
            levels: 1,
            stride: 0,
            packaged_block: None,
        };
        let id = t.create(d, GpuAddr::new(0x1_0000).unwrap()).unwrap();
        (t, id)
    }

    fn sampler() -> Tsc {
        Tsc::decode(&[0; 8])
    }

    #[test]
    fn a_handle_resolves_to_its_texture_and_sampler() {
        let (table, tex) = table_with_one();
        let mut d = Descriptors::new();
        d.add_texture_pool(0, 100, GpuAddr::new(0x10_0000).unwrap())
            .unwrap();
        d.add_sampler_pool(0, 10, GpuAddr::new(0x20_0000).unwrap())
            .unwrap();
        d.register_texture(&table, 42, tex, TextureView::identity())
            .unwrap();
        d.register_sampler(3, sampler()).unwrap();
        let h = Descriptors::handle(42, 3);
        assert_eq!(h, 42 | 3 << 20);
        let (slot, samp) = d.resolve(h).unwrap();
        assert_eq!(slot.texture, tex);
        assert_eq!(slot.header.address.get(), 0x1_0000);
        assert!(samp.is_some());
        assert!(d.resolve(Descriptors::handle(42, 4)).unwrap().1.is_none());
        assert_eq!(
            d.resolve(Descriptors::handle(41, 3)).unwrap_err(),
            NvnError::UnknownDescriptor(41)
        );
    }

    #[test]
    fn pools_share_one_id_space_and_must_not_overlap() {
        let mut d = Descriptors::new();
        let m = GpuAddr::new(0x1000).unwrap();
        d.add_texture_pool(0, 100, m).unwrap();
        d.add_texture_pool(100, 50, m).unwrap();
        assert!(d.add_texture_pool(149, 2, m).is_err());
        assert!(d.add_texture_pool(500, 0, m).is_err());
        assert!(d.add_texture_pool(MAX_TEXTURE_ID, 2, m).is_err());
        assert!(d.add_sampler_pool(MAX_SAMPLER_ID, 1, m).is_ok());
        assert!(d.add_sampler_pool(MAX_SAMPLER_ID, 1, m).is_err());
        assert_eq!(d.texture_pool_of(120).unwrap().base, 100);
        assert!(d.texture_pool_of(150).is_none());
    }

    #[test]
    fn registering_outside_a_pool_or_with_a_dead_texture_fails() {
        let (mut table, tex) = table_with_one();
        let mut d = Descriptors::new();
        d.add_texture_pool(10, 5, GpuAddr::new(0x10_0000).unwrap())
            .unwrap();
        assert!(
            d.register_texture(&table, 9, tex, TextureView::identity())
                .is_err()
        );
        d.register_texture(&table, 10, tex, TextureView::identity())
            .unwrap();
        d.drop_texture(tex);
        assert!(d.texture(10).is_err());
        table.finalize(tex).unwrap();
        assert!(
            d.register_texture(&table, 10, tex, TextureView::identity())
                .is_err()
        );
        assert!(d.register_sampler(0, sampler()).is_err());
    }

    #[test]
    fn the_header_lives_32_bytes_per_slot_in_the_pool_memory() {
        let mut d = Descriptors::new();
        d.add_texture_pool(8, 4, GpuAddr::new(0x4_0000).unwrap())
            .unwrap();
        let p = d.texture_pool_of(10).unwrap();
        assert_eq!(p.header_address(10).unwrap().get(), 0x4_0040);
    }
}
