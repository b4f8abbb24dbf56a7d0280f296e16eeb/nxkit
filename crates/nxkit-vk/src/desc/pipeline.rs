//! The descriptions of render passes, framebuffers and pipelines.

use std::sync::Arc;

use crate::format::VertexFormat;

use super::{PipelineLayoutDesc, Rect2D, ShaderKey, TextureId, Viewport};

/// One attachment of a render pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AttachmentDesc {
    /// The `VkFormat`.
    pub format: u32,
    /// The samples.
    pub samples: u32,
    /// `loadOp` (`VkAttachmentLoadOp`: 0 load, 1 clear, 2 don't care).
    pub load: u32,
    /// `storeOp` (`VkAttachmentStoreOp`: 0 store, 1 don't care).
    pub store: u32,
    /// `stencilLoadOp`.
    pub stencil_load: u32,
    /// `stencilStoreOp`.
    pub stencil_store: u32,
}

/// A render pass with its one subpass. A render pass with more subpasses is refused
/// (`VK_ERROR_FEATURE_NOT_PRESENT`), and so are input attachments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RenderPassDesc {
    /// The attachments.
    pub attachments: Vec<AttachmentDesc>,
    /// The colour attachments of the subpass, as attachment indices (None for `VK_ATTACHMENT_UNUSED`), at most 8.
    pub colors: Vec<Option<u32>>,
    /// The resolve attachment of each colour attachment, or empty.
    pub resolves: Vec<Option<u32>>,
    /// The depth-stencil attachment.
    pub depth: Option<u32>,
}

impl RenderPassDesc {
    /// The `VkFormat` of each colour attachment of the subpass, None for an unused one.
    pub fn color_formats(&self) -> Vec<Option<u32>> {
        self.colors
            .iter()
            .map(|c| {
                c.and_then(|i| self.attachments.get(i as usize))
                    .map(|a| a.format)
            })
            .collect()
    }

    /// The `VkFormat` of the depth-stencil attachment.
    pub fn depth_format(&self) -> Option<u32> {
        self.depth
            .and_then(|i| self.attachments.get(i as usize))
            .map(|a| a.format)
    }
}

/// One attachment of a framebuffer: an image view, resolved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FramebufferAttachment {
    /// The texture of the view.
    pub view: TextureId,
    /// The texture of its image.
    pub image: TextureId,
    /// The `VkFormat` of the view.
    pub format: u32,
    /// The samples of the image.
    pub samples: u32,
    /// The first level of the view.
    pub level: u32,
    /// The first layer of the view.
    pub layer: u32,
}

/// A framebuffer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FramebufferDesc {
    /// The attachments, at most 16.
    pub attachments: Vec<FramebufferAttachment>,
    /// The width.
    pub width: u32,
    /// The height.
    pub height: u32,
    /// The layers.
    pub layers: u32,
}

/// One shader stage of a pipeline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageDesc {
    /// The `VkShaderStageFlagBits`.
    pub stage: u32,
    /// The key of the module.
    pub key: ShaderKey,
    /// The entry point name.
    pub entry: String,
    /// The specialization constants: id and value bytes. The backend sets the ones that the function has.
    pub specialization: Vec<(u32, Vec<u8>)>,
}

/// One vertex buffer binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VertexBinding {
    /// The binding number (at most 13).
    pub binding: u32,
    /// The stride. A stride of 0 means one value for all vertices.
    pub stride: u32,
    /// Whether the binding steps per instance.
    pub per_instance: bool,
}

/// One vertex attribute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VertexAttribute {
    /// The shader location (at most 30).
    pub location: u32,
    /// The binding it reads.
    pub binding: u32,
    /// The `VkFormat`.
    pub format: u32,
    /// The vertex format, None when the format is not a vertex format (the backend reports it).
    pub vertex: Option<VertexFormat>,
    /// The offset in the element.
    pub offset: u32,
}

/// The stencil state of one face (`VkStencilOpState`, raw values: `VkStencilOp` and `VkCompareOp`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct StencilFace {
    /// `failOp`.
    pub fail: u32,
    /// `passOp`.
    pub pass: u32,
    /// `depthFailOp`.
    pub depth_fail: u32,
    /// `compareOp`.
    pub compare: u32,
    /// `compareMask`.
    pub compare_mask: u32,
    /// `writeMask`.
    pub write_mask: u32,
    /// `reference`.
    pub reference: u32,
}

/// The depth and stencil state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct DepthStencil {
    /// `depthTestEnable`.
    pub depth_test: bool,
    /// `depthWriteEnable`.
    pub depth_write: bool,
    /// `depthCompareOp`.
    pub depth_compare: u32,
    /// `depthBoundsTestEnable`: not implemented, reported.
    pub bounds_test: bool,
    /// `stencilTestEnable`.
    pub stencil_test: bool,
    /// The front face.
    pub front: StencilFace,
    /// The back face.
    pub back: StencilFace,
}

/// The blend state of one colour attachment (raw `VkBlendFactor`, `VkBlendOp`, `VkColorComponentFlags`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct BlendAttachment {
    /// `blendEnable`.
    pub enable: bool,
    /// `srcColorBlendFactor`.
    pub src_color: u32,
    /// `dstColorBlendFactor`.
    pub dst_color: u32,
    /// `colorBlendOp`.
    pub color_op: u32,
    /// `srcAlphaBlendFactor`.
    pub src_alpha: u32,
    /// `dstAlphaBlendFactor`.
    pub dst_alpha: u32,
    /// `alphaBlendOp`.
    pub alpha_op: u32,
    /// `colorWriteMask` (Vulkan bits: R 1, G 2, B 4, A 8).
    pub write_mask: u32,
}

impl BlendAttachment {
    /// Whether a factor reads the second source colour (dual-source blending).
    pub fn dual_source(&self) -> bool {
        let src1 = |f: u32| (15..=18).contains(&f);
        self.enable
            && (src1(self.src_color)
                || src1(self.dst_color)
                || src1(self.src_alpha)
                || src1(self.dst_alpha))
    }
}

/// The rasterization state.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Raster {
    /// `depthClampEnable`.
    pub depth_clamp: bool,
    /// `rasterizerDiscardEnable`.
    pub discard: bool,
    /// `polygonMode` (0 fill, 1 line, 2 point).
    pub polygon_mode: u32,
    /// `cullMode` (bits: 1 front, 2 back).
    pub cull_mode: u32,
    /// `frontFace` (0 counter-clockwise, 1 clockwise).
    pub front_face: u32,
    /// The depth bias (constant factor, clamp, slope factor), when it is on.
    pub depth_bias: Option<[f32; 3]>,
    /// `lineWidth`.
    pub line_width: f32,
}

/// The fixed-function state of a graphics pipeline.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PipelineState {
    /// The vertex buffer bindings.
    pub bindings: Vec<VertexBinding>,
    /// The vertex attributes.
    pub attributes: Vec<VertexAttribute>,
    /// `VkPrimitiveTopology` (3 = triangle list when the game gives none).
    pub topology: u32,
    /// `primitiveRestartEnable`.
    pub primitive_restart: bool,
    /// The viewport count (at least 1).
    pub viewport_count: u32,
    /// The static viewports (empty when the viewport is dynamic).
    pub viewports: Vec<Viewport>,
    /// The static scissors (empty when the scissor is dynamic).
    pub scissors: Vec<Rect2D>,
    /// The rasterization state.
    pub raster: Raster,
    /// `rasterizationSamples` (at least 1).
    pub samples: u32,
    /// `alphaToCoverageEnable`.
    pub alpha_to_coverage: bool,
    /// The depth and stencil state, None when the game gives none.
    pub depth_stencil: Option<DepthStencil>,
    /// The logic op when it is on and not `COPY` (not implemented, reported at draw).
    pub logic_op: Option<u32>,
    /// The blend state per colour attachment.
    pub blend: Vec<BlendAttachment>,
    /// `blendConstants`.
    pub blend_constants: [f32; 4],
    /// The `VkDynamicState` values.
    pub dynamic: Vec<u32>,
}

impl PipelineState {
    /// Whether a `VkDynamicState` is dynamic.
    pub fn is_dynamic(&self, s: u32) -> bool {
        self.dynamic.contains(&s)
    }

    /// Whether a blend factor reads the second source colour.
    pub fn dual_source(&self) -> bool {
        self.blend.iter().any(BlendAttachment::dual_source)
    }
}

/// The attachment formats that a graphics pipeline draws into: those of subpass 0 of its render pass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Targets {
    /// The `VkFormat` of each colour attachment, None for an unused one.
    pub colors: Vec<Option<u32>>,
    /// The `VkFormat` of the depth-stencil attachment.
    pub depth: Option<u32>,
}

/// A graphics pipeline as the backend builds it.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphicsPipelineDesc {
    /// The stages (vertex, fragment, and a geometry stage that is kept for the backend to lower or refuse).
    pub stages: Vec<StageDesc>,
    /// The fixed-function state.
    pub state: PipelineState,
    /// The attachment formats.
    pub targets: Targets,
    /// The pipeline layout.
    pub layout: Arc<PipelineLayoutDesc>,
}

/// A compute pipeline as the backend builds it. It uses the shader's full local size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComputePipelineDesc {
    /// The compute stage.
    pub stage: StageDesc,
    /// The pipeline layout.
    pub layout: Arc<PipelineLayoutDesc>,
}
