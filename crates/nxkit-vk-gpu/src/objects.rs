//! The Metal objects of the backend, by the Vulkan handle of the object they were made for.
//!
//! The ids never repeat (`nxkit-vk` hands out handles that are never reused), so a lookup of a destroyed object finds
//! nothing and the command that names it binds nothing. A submit resolves the ids it needs while it encodes, and the
//! command buffer keeps what it encodes alive. A destroy after the submit is safe.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use nxkit_abi::vk_table::Resource;
use nxkit_metal::{
    ComputePipeline, CullMode, DepthClip, DepthStencilDesc, DepthStencilState, FillMode,
    GuestMemory, Primitive, RenderPipeline, Sampler, Texture, Winding,
};
use nxkit_vk::desc::{MemoryId, PipelineId, Rect2D, SamplerId, TextureId, Viewport};
use nxkit_vk::format::Conversion;

use crate::fixed::Masks;
use crate::vertex::VertexPlan;

/// A texture of an image whose packed format the host unpacks (`Conversion`): its own texture, filled from the bound
/// memory before each submit that follows a change.
pub(crate) struct Converted {
    /// The memory the image is bound to.
    pub(crate) memory: Arc<GuestMemory>,
    /// The offset of the first texel in it.
    pub(crate) offset: u64,
    /// The bytes from one row to the next.
    pub(crate) pitch: u64,
    /// The unpacking.
    pub(crate) conversion: Conversion,
    /// The bytes of the memory as last unpacked.
    pub(crate) shadow: Vec<u8>,
}

/// A texture: an image, a view or a swapchain image.
pub(crate) struct Tex {
    /// The Metal texture.
    pub(crate) texture: Texture,
    /// The memory that a linear image lies over, kept alive with the texture.
    pub(crate) _memory: Option<Arc<GuestMemory>>,
    /// The unpacking of a converted linear image.
    pub(crate) converted: Option<Mutex<Converted>>,
}

/// What a draw needs of a graphics pipeline, besides the Metal pipeline state.
pub(crate) struct Graphics {
    /// The render pipeline.
    pub(crate) pipeline: RenderPipeline,
    /// The depth and stencil state.
    pub(crate) depth_stencil: DepthStencilState,
    /// The description of `depth_stencil`, for the states of dynamic stencil masks. None when stencil masks are not
    /// dynamic.
    pub(crate) dynamic_masks: Option<(DepthStencilDesc, Masks)>,
    /// The states that dynamic stencil masks made, by masks.
    pub(crate) mask_states: Mutex<HashMap<Masks, DepthStencilState>>,
    /// The primitive of the draws.
    pub(crate) primitive: Primitive,
    /// The faces that are culled.
    pub(crate) cull: CullMode,
    /// The front face winding.
    pub(crate) winding: Winding,
    /// The fill mode.
    pub(crate) fill: FillMode,
    /// The depth clip mode.
    pub(crate) clip: DepthClip,
    /// The depth bias of the pipeline (constant, clamp, slope).
    pub(crate) bias: Option<[f32; 3]>,
    /// The blend constants of the pipeline.
    pub(crate) blend_constants: [f32; 4],
    /// The stencil reference values of the pipeline (front, back).
    pub(crate) stencil_ref: (u32, u32),
    /// The static viewport, None when it is dynamic.
    pub(crate) viewport: Option<Viewport>,
    /// The static scissor, None when it is dynamic.
    pub(crate) scissor: Option<Rect2D>,
    /// Dynamic states: depth bias, blend constants, stencil reference.
    pub(crate) dynamic_bias: bool,
    /// Whether the blend constants are dynamic.
    pub(crate) dynamic_blend: bool,
    /// Whether the stencil reference is dynamic.
    pub(crate) dynamic_ref: bool,
    /// The vertex input.
    pub(crate) vertex: VertexPlan,
    /// The resources of the vertex and the fragment shader.
    pub(crate) resources: [Arc<[Resource]>; 2],
    /// The bytes of the push constants of the layout.
    pub(crate) push_size: u32,
    /// A logic op that Metal does not do (reported at draw).
    pub(crate) logic_op: Option<u32>,
    /// The keys of the shaders, for messages.
    pub(crate) name: String,
}

/// What a dispatch needs of a compute pipeline.
pub(crate) struct Compute {
    /// The compute pipeline.
    pub(crate) pipeline: ComputePipeline,
    /// The threadgroup size of the kernel.
    pub(crate) threads: [u32; 3],
    /// The resources of the kernel.
    pub(crate) resources: Arc<[Resource]>,
    /// The bytes of the push constants of the layout.
    pub(crate) push_size: u32,
}

/// A pipeline.
#[derive(Clone)]
pub(crate) enum Pipe {
    /// A graphics pipeline.
    Graphics(Arc<Graphics>),
    /// A compute pipeline.
    Compute(Arc<Compute>),
}

/// The objects.
#[derive(Default)]
pub(crate) struct Objects {
    /// The storage of each `VkDeviceMemory` that has one.
    pub(crate) memories: HashMap<MemoryId, Arc<GuestMemory>>,
    /// The textures.
    pub(crate) textures: HashMap<TextureId, Arc<Tex>>,
    /// The samplers.
    pub(crate) samplers: HashMap<SamplerId, Sampler>,
    /// The pipelines that were built.
    pub(crate) pipelines: HashMap<PipelineId, Pipe>,
    /// The converted linear images, which each submit checks.
    pub(crate) converted: Vec<TextureId>,
}

impl Objects {
    /// The GPU address of byte `offset` of a memory, when the memory has storage and the offset is inside it.
    pub(crate) fn address(&self, memory: MemoryId, offset: u64) -> Option<u64> {
        self.memories
            .get(&memory)?
            .gpu_address_of_offset(offset, 1)
            .ok()
    }

    /// The Metal texture of an id.
    pub(crate) fn texture(&self, id: TextureId) -> Option<&Texture> {
        self.textures.get(&id).map(|t| &t.texture)
    }

    /// The graphics pipeline of an id.
    pub(crate) fn graphics(&self, id: PipelineId) -> Option<Arc<Graphics>> {
        match self.pipelines.get(&id) {
            Some(Pipe::Graphics(g)) => Some(g.clone()),
            _ => None,
        }
    }

    /// The compute pipeline of an id.
    pub(crate) fn compute(&self, id: PipelineId) -> Option<Arc<Compute>> {
        match self.pipelines.get(&id) {
            Some(Pipe::Compute(c)) => Some(c.clone()),
            _ => None,
        }
    }
}
