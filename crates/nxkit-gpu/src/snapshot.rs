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
}

/// A copy of the records and handles that one frame uses.
#[derive(Debug, Default)]
pub struct Snapshot {
    records: crate::draws::FastMap<TexId, TextureRecord>,
    handles: crate::draws::FastMap<u64, (TextureSlot, Option<Tsc>)>,
}

impl Snapshot {
    /// Copies what the encode of `frame` reads: the records of the textures that its commands and attachments name, the
    /// slots of the handles that `detail` binds and the records behind those slots.
    pub fn capture(
        frame: &Frame,
        plan: &PassPlan,
        detail: &FrameDetail,
        table: &ResourceTable,
        descriptors: &Descriptors,
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
            .chain(detail.dispatches.iter().flat_map(|d| d.textures.iter()))
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
        Self { records, handles }
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
        );
        table.finalize(id).unwrap();
        assert!(table.get(id).is_err());
        assert_eq!(snap.record(id).unwrap().id, id);
        assert!(snap.resolve(5).is_err());
    }
}
