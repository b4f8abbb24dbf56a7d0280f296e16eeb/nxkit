//! What the encoder reads from the front end: the texture records and the texture descriptors.
//!
//! [`Gpu::encode_frame`](crate::Gpu::encode_frame) reads them through [`EncodeView`]. The synchronous path lends the
//! live tables of the front end ([`Live`]). The encoder thread cannot hold them, because the front end changes them
//! while the encode runs. It encodes from a [`Snapshot`] that holds only the records and descriptors that one frame
//! names.

use nxkit_nvn::descriptors::{Descriptors, MAX_TEXTURE_ID, TextureSlot};
use nxkit_nvn::error::{NvnError, Result};
use nxkit_nvn::frame::Frame;
use nxkit_nvn::passes::PassPlan;
use nxkit_nvn::resource::{ResourceTable, TexId, TextureRecord};
use nxkit_nvn::tic::Tsc;

use crate::detail::FrameDetail;
use crate::gpu::collect_uses;

/// The reads of the encoder: a texture record by id and a texture handle by value.
pub trait EncodeView: Sync {
    /// The record of a live texture.
    fn record(&self, id: TexId) -> Result<&TextureRecord>;

    /// The texture slot of a handle, and its sampler when one is registered.
    fn resolve(&self, handle: u64) -> Result<(&TextureSlot, Option<&Tsc>)>;

    /// The texture slot of descriptor id `id`: what a shader reads through the texture pool table.
    fn texture_slot(&self, id: u32) -> Result<&TextureSlot>;

    /// The highest id of the texture pools, or `None` when the game made none.
    fn texture_limit(&self) -> Option<u32>;

    /// The sampler counter of the descriptors ([`Descriptors::sampler_epoch`]).
    fn sampler_epoch(&self) -> u64;

    /// The registered samplers by id. A [`Snapshot`] lists them only when the sampler table did not hold them yet at
    /// the flush; the encoder asks when [`EncodeView::sampler_epoch`] is newer than the table.
    fn samplers(&self) -> Vec<(u32, Tsc)>;

    /// The texture ids that the encoder resolves for the pool table. A snapshot lists the ids it carries slots for;
    /// `None` (the live tables) lets the encoder use the ids it read from the bitmap in this frame.
    fn pool_ids(&self) -> Option<&[u32]> {
        None
    }
}

/// The live tables of the front end, for the synchronous encode.
pub struct Live<'a> {
    /// The textures.
    pub table: &'a ResourceTable,
    /// The descriptor pools.
    pub descriptors: &'a Descriptors,
}

impl EncodeView for Live<'_> {
    fn record(&self, id: TexId) -> Result<&TextureRecord> {
        self.table.get(id)
    }

    fn resolve(&self, handle: u64) -> Result<(&TextureSlot, Option<&Tsc>)> {
        self.descriptors.resolve(handle)
    }

    fn texture_slot(&self, id: u32) -> Result<&TextureSlot> {
        self.descriptors.texture(id)
    }

    fn texture_limit(&self) -> Option<u32> {
        self.descriptors.texture_limit()
    }

    fn sampler_epoch(&self) -> u64 {
        self.descriptors.sampler_epoch()
    }

    fn samplers(&self) -> Vec<(u32, Tsc)> {
        self.descriptors.samplers().map(|(i, t)| (i, *t)).collect()
    }
}

/// A copy of the records and handles that one frame uses.
#[derive(Debug, Default)]
pub struct Snapshot {
    records: crate::draws::FastMap<TexId, TextureRecord>,
    handles: crate::draws::FastMap<u64, (TextureSlot, Option<Tsc>)>,
    /// The ids that the pool table needs, and the slots of those that name a texture.
    pool_ids: Vec<u32>,
    pool_slots: crate::draws::FastMap<u32, TextureSlot>,
    texture_limit: Option<u32>,
    sampler_epoch: u64,
    /// The registered samplers, when the sampler table did not hold them at the flush.
    samplers: Vec<(u32, Tsc)>,
}

impl Snapshot {
    /// Copies what the encode of `frame` reads: the records of the textures that its commands and attachments name, the
    /// slots of the handles that `detail` binds and the records behind those slots.
    ///
    /// `pool_ids` are the texture ids that shaders read through the pool table (the encoder publishes them each
    /// frame): their slots and records are copied too. `samplers_done` is the sampler epoch that the sampler table
    /// holds (`u64::MAX` for none): the registered samplers are copied when it is older than the descriptors'.
    pub fn capture(
        frame: &Frame,
        plan: &PassPlan,
        detail: &FrameDetail,
        table: &ResourceTable,
        descriptors: &Descriptors,
        pool_ids: &[u32],
        samplers_done: u64,
    ) -> Self {
        let mut records = crate::draws::FastMap::default();
        let mut keep = |id: TexId| {
            if let Ok(r) = table.get(id) {
                records.insert(id, *r);
            }
        };
        for id in collect_uses(frame, plan).into_keys() {
            keep(id);
        }
        let mut handles = crate::draws::FastMap::default();
        let wanted = detail
            .draws
            .iter()
            .flat_map(|d| d.textures.iter())
            .chain(
                detail
                    .dispatches
                    .iter()
                    .flat_map(|d| d.textures.iter().chain(d.images.iter())),
            )
            .map(|t| t.handle)
            .filter(|&h| h != 0);
        for handle in wanted {
            if handles.contains_key(&handle) {
                continue;
            }
            if let Ok((slot, sampler)) = descriptors.resolve(handle) {
                keep(slot.texture);
                handles.insert(handle, (*slot, sampler.copied()));
            }
        }
        let mut pool_slots = crate::draws::FastMap::default();
        for &id in pool_ids {
            if let Ok(slot) = descriptors.texture(id) {
                keep(slot.texture);
                pool_slots.insert(id, *slot);
            }
        }
        let sampler_epoch = descriptors.sampler_epoch();
        let samplers = if samplers_done == u64::MAX || sampler_epoch > samplers_done {
            descriptors.samplers().map(|(i, t)| (i, *t)).collect()
        } else {
            Vec::new()
        };
        Self {
            records,
            handles,
            pool_ids: pool_ids.to_vec(),
            pool_slots,
            texture_limit: descriptors.texture_limit(),
            sampler_epoch,
            samplers,
        }
    }
}

impl EncodeView for Snapshot {
    fn record(&self, id: TexId) -> Result<&TextureRecord> {
        self.records.get(&id).ok_or(NvnError::UnknownTexture(id.0))
    }

    fn resolve(&self, handle: u64) -> Result<(&TextureSlot, Option<&Tsc>)> {
        self.handles
            .get(&handle)
            .map(|(slot, sampler)| (slot, sampler.as_ref()))
            .ok_or(NvnError::UnknownDescriptor(
                (handle & u64::from(MAX_TEXTURE_ID)) as u32,
            ))
    }

    fn texture_slot(&self, id: u32) -> Result<&TextureSlot> {
        self.pool_slots
            .get(&id)
            .ok_or(NvnError::UnknownDescriptor(id))
    }

    fn texture_limit(&self) -> Option<u32> {
        self.texture_limit
    }

    fn sampler_epoch(&self) -> u64 {
        self.sampler_epoch
    }

    fn samplers(&self) -> Vec<(u32, Tsc)> {
        self.samplers.clone()
    }

    fn pool_ids(&self) -> Option<&[u32]> {
        Some(&self.pool_ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxkit_nvn::layout::{Target, TextureDesc};
    use nxkit_nvn::space::GpuAddr;

    #[test]
    fn a_snapshot_keeps_the_record_that_the_table_later_changes() {
        let mut table = ResourceTable::new();
        let desc = TextureDesc {
            target: Target::D2,
            format: 0x25,
            flags: 0,
            samples: 0,
            width: 8,
            height: 8,
            depth: 1,
            levels: 1,
            stride: 0,
            packaged_block: None,
        };
        let id = table.create(desc, GpuAddr::new(0x1000).unwrap()).unwrap();
        let mut frame = Frame::new();
        frame.cmds.push(nxkit_nvn::frame::Cmd::Present(id));
        let snap = Snapshot::capture(
            &frame,
            &PassPlan::default(),
            &FrameDetail::default(),
            &table,
            &Descriptors::new(),
            &[],
            u64::MAX,
        );
        table.finalize(id).unwrap();
        assert!(table.get(id).is_err());
        assert_eq!(snap.record(id).unwrap().id, id);
        assert!(snap.resolve(5).is_err());
    }
}
