//! The front end: the one object that the NVN entry points of the host call.
//!
//! The front end owns the resource table, the descriptor pools, one recorder per command buffer and the frame.
//! It turns guest addresses into GPU addresses through a [`GpuSpace`] and rejects addresses outside the mapping.
//! A frame ends at a present. [`FrontEnd::end_frame`] runs the pass analysis and keeps the frame for the backend.
//! The next [`FrontEnd::begin_frame`] drops the frame and keeps its memory.

use crate::analyse::Analyser;
use crate::descriptors::{Descriptors, TextureSlot};
use crate::error::{NvnError, Result};
use crate::frame::Frame;
use crate::layout::TextureDesc;
use crate::passes::PassPlan;
use crate::recorder::Recorder;
use crate::resource::{ResourceTable, TexId};
use crate::space::{GpuAddr, GpuSpace};
use crate::tic::{TextureView, Tsc};

/// The NVN front end.
#[derive(Debug)]
pub struct FrontEnd<S: GpuSpace> {
    space: S,
    table: ResourceTable,
    descriptors: Descriptors,
    recorders: Vec<Recorder>,
    frame: Frame,
    plan: PassPlan,
    analyser: Analyser,
    hit: Vec<TexId>,
}

impl<S: GpuSpace> FrontEnd<S> {
    /// A front end over a guest memory mapping.
    pub fn new(space: S) -> Self {
        Self {
            space,
            table: ResourceTable::new(),
            descriptors: Descriptors::new(),
            recorders: Vec::new(),
            frame: Frame::new(),
            plan: PassPlan::new(),
            analyser: Analyser::new(),
            hit: Vec::new(),
        }
    }

    /// The address space.
    pub fn space(&self) -> &S {
        &self.space
    }

    /// The textures.
    pub fn resources(&self) -> &ResourceTable {
        &self.table
    }

    /// The textures, for the host to set flags.
    pub fn resources_mut(&mut self) -> &mut ResourceTable {
        &mut self.table
    }

    /// The descriptor pools.
    pub fn descriptors(&self) -> &Descriptors {
        &self.descriptors
    }

    /// The GPU address of guest memory. This is `nvnMemoryPoolGetBufferAddress` and its relatives.
    pub fn gpu_address(&self, host: u64, len: u64) -> Result<GpuAddr> {
        self.space.gpu_address(host, len)
    }

    /// Creates a texture in guest memory. This is `nvnTextureInitialize`.
    pub fn create_texture(&mut self, desc: TextureDesc, host_address: u64) -> Result<TexId> {
        let size = desc.layout()?.size;
        let address = self.space.gpu_address(host_address, size)?;
        self.table.create(desc, address)
    }

    /// Destroys a texture. This is `nvnTextureFinalize`.
    pub fn finalize_texture(&mut self, id: TexId) -> Result<()> {
        self.table.finalize(id)?;
        self.descriptors.drop_texture(id);
        Ok(())
    }

    /// Creates a texture pool. This is `nvnTexturePoolInitialize`.
    pub fn add_texture_pool(&mut self, base: u32, count: u32, host_memory: u64) -> Result<()> {
        let bytes = u64::from(count) * crate::tic::HEADER_BYTES as u64;
        let memory = self.space.gpu_address(host_memory, bytes)?;
        self.descriptors.add_texture_pool(base, count, memory)
    }

    /// Creates a sampler pool. This is `nvnSamplerPoolInitialize`.
    pub fn add_sampler_pool(&mut self, base: u32, count: u32, host_memory: u64) -> Result<()> {
        let bytes = u64::from(count) * crate::tic::HEADER_BYTES as u64;
        let memory = self.space.gpu_address(host_memory, bytes)?;
        self.descriptors.add_sampler_pool(base, count, memory)
    }

    /// Registers a texture. This is `nvnTexturePoolRegisterTexture`.
    pub fn register_texture(
        &mut self,
        id: u32,
        texture: TexId,
        view: TextureView,
    ) -> Result<TextureSlot> {
        self.descriptors
            .register_texture(&self.table, id, texture, view)
    }

    /// Registers a sampler. This is `nvnSamplerPoolRegisterSampler`.
    pub fn register_sampler(&mut self, id: u32, sampler: Tsc) -> Result<()> {
        self.descriptors.register_sampler(id, sampler)
    }

    /// The recorder of a command buffer. It is made at the first use.
    pub fn recorder(&mut self, cb: usize) -> &mut Recorder {
        if self.recorders.len() <= cb {
            self.recorders.resize_with(cb + 1, Recorder::new);
        }
        &mut self.recorders[cb]
    }

    /// Binds the texture of a handle to a sampling slot of a command buffer. This is `nvnCommandBufferBindTexture`.
    pub fn bind_texture(&mut self, cb: usize, slot: usize, handle: u64) -> Result<()> {
        let tex = self.descriptors.resolve(handle)?.0.texture;
        self.recorder(cb).bind_texture(slot, Some(tex));
        Ok(())
    }

    /// Binds the texture of a handle to an image slot of a command buffer. This is `nvnCommandBufferBindImage`.
    pub fn bind_image(&mut self, cb: usize, slot: usize, handle: u64) -> Result<()> {
        let tex = self.descriptors.resolve(handle)?.0.texture;
        self.recorder(cb).bind_image(slot, Some(tex));
        Ok(())
    }

    /// Submits a command buffer to the queue. The recorder keeps its commands, so the game can submit it again.
    pub fn submit(&mut self, cb: usize) -> Result<()> {
        let r = self
            .recorders
            .get(cb)
            .ok_or(NvnError::BadPool("unknown command buffer"))?;
        self.frame.append(r.stream());
        Ok(())
    }

    /// A present ends the frame. This runs the pass analysis and returns its result.
    pub fn end_frame(&mut self) -> Result<&PassPlan> {
        self.analyser
            .analyse(&self.frame, &self.table, &mut self.plan)?;
        Ok(&self.plan)
    }

    /// The frame that the last `end_frame` analysed.
    pub fn frame(&self) -> &Frame {
        &self.frame
    }

    /// The result of the last `end_frame`.
    pub fn plan(&self) -> &PassPlan {
        &self.plan
    }

    /// Starts the next frame.
    pub fn begin_frame(&mut self) {
        self.frame.clear();
    }

    /// Tells that the guest wrote host memory. The textures that overlap are marked `GuestNewer` and returned.
    pub fn guest_wrote(&mut self, host: u64, len: u64) -> Result<&[TexId]> {
        let start = self.space.gpu_address(host, len)?;
        self.hit.clear();
        self.table.guest_wrote(start, len, &mut self.hit);
        Ok(&self.hit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Target;
    use crate::passes::{LoadAction, StoreAction};
    use crate::resource::Residency;
    use crate::space::LinearSpace;

    const HOST: u64 = 0x7000_0000;
    const GPU: u64 = 0x15_0400_8000;

    fn fe() -> FrontEnd<LinearSpace> {
        FrontEnd::new(LinearSpace::new(HOST, GPU, 64 << 20).unwrap())
    }

    fn rgba() -> TextureDesc {
        TextureDesc {
            target: Target::D2,
            format: 0x25,
            flags: 0,
            samples: 0,
            width: 64,
            height: 64,
            depth: 1,
            levels: 1,
            stride: 0,
            packaged_block: None,
        }
    }

    #[test]
    fn a_texture_gets_the_metal_address_of_its_memory() {
        let mut f = fe();
        let t = f.create_texture(rgba(), HOST + 0x1_0000).unwrap();
        assert_eq!(f.resources().get(t).unwrap().address.get(), GPU + 0x1_0000);
        assert!(f.create_texture(rgba(), HOST + (64 << 20)).is_err());
        assert!(f.create_texture(rgba(), HOST - 4096).is_err());
    }

    #[test]
    fn one_frame_from_handle_to_plan() {
        let mut f = fe();
        let scene = f.create_texture(rgba(), HOST + 0x10_0000).unwrap();
        let out = f.create_texture(rgba(), HOST + 0x20_0000).unwrap();
        f.add_texture_pool(0, 16, HOST + 0x30_0000).unwrap();
        f.add_sampler_pool(0, 4, HOST + 0x31_0000).unwrap();
        f.register_texture(1, scene, TextureView::identity())
            .unwrap();
        f.register_sampler(0, Tsc::decode(&[0; 8])).unwrap();
        let h = Descriptors::handle(1, 0);
        f.recorder(0).begin();
        f.recorder(0).set_render_targets(&[scene], None);
        f.recorder(0).clear_color(0);
        f.recorder(0).draw_now();
        f.recorder(0).set_render_targets(&[out], None);
        f.bind_texture(0, 0, h).unwrap();
        f.recorder(0).draw_now();
        f.recorder(0).present(out);
        f.begin_frame();
        f.submit(0).unwrap();
        let plan = f.end_frame().unwrap();
        assert_eq!(plan.stats.render_passes, 2);
        assert_eq!(plan.attachments[0].load, LoadAction::Clear);
        assert_eq!(plan.attachments[0].store, StoreAction::Store);
        assert!(!plan.attachments[0].store_assumed);
        assert_eq!(plan.attachments[1].load, LoadAction::Load);
        assert!(f.bind_texture(0, 0, Descriptors::handle(2, 0)).is_err());
    }

    #[test]
    fn a_command_buffer_can_be_submitted_in_every_frame() {
        let mut f = fe();
        let c = f.create_texture(rgba(), HOST + 0x10_0000).unwrap();
        f.recorder(0).begin();
        f.recorder(0).set_render_targets(&[c], None);
        f.recorder(0).clear_color(0);
        f.recorder(0).draw_now();
        for _ in 0..2 {
            f.begin_frame();
            f.submit(0).unwrap();
            f.submit(0).unwrap();
            assert_eq!(f.end_frame().unwrap().stats.passes, 2);
        }
        assert!(f.submit(5).is_err());
    }

    #[test]
    fn a_cpu_write_to_texture_memory_is_found() {
        let mut f = fe();
        let t = f.create_texture(rgba(), HOST + 0x10_0000).unwrap();
        f.resources_mut().synced(t).unwrap();
        assert!(f.guest_wrote(HOST + 0x20_0000, 64).unwrap().is_empty());
        assert_eq!(f.guest_wrote(HOST + 0x10_0100, 64).unwrap(), [t]);
        assert_eq!(
            f.resources().get(t).unwrap().residency,
            Residency::GuestNewer
        );
        assert!(f.guest_wrote(HOST - 1, 64).is_err());
    }

    #[test]
    fn finalizing_a_texture_drops_its_descriptor() {
        let mut f = fe();
        let t = f.create_texture(rgba(), HOST + 0x10_0000).unwrap();
        f.add_texture_pool(0, 4, HOST + 0x30_0000).unwrap();
        f.register_texture(0, t, TextureView::identity()).unwrap();
        f.finalize_texture(t).unwrap();
        assert!(f.bind_texture(0, 0, Descriptors::handle(0, 0)).is_err());
    }
}
