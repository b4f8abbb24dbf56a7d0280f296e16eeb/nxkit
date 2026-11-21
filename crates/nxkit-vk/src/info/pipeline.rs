//! Decoders of render passes, framebuffers, shader modules and pipelines.

use crate::consts::special::ATTACHMENT_UNUSED;
use crate::desc::{
    AttachmentDesc, BlendAttachment, DepthStencil, PipelineState, Raster, RenderPassDesc,
    StencilFace, VertexAttribute, VertexBinding,
};
use crate::format;
use crate::layout::{
    attachment_description as ad, attachment_reference as ar, compute_pipeline_create_info as cpc,
    framebuffer_create_info as fbc, graphics_pipeline_create_info as gpc,
    pipeline_color_blend_attachment_state as pcba, pipeline_color_blend_state_create_info as pcb,
    pipeline_depth_stencil_state_create_info as pds, pipeline_dynamic_state_create_info as pdy,
    pipeline_input_assembly_state_create_info as pia,
    pipeline_multisample_state_create_info as pms, pipeline_rasterization_state_create_info as prs,
    pipeline_shader_stage_create_info as pss, pipeline_vertex_input_state_create_info as pvi,
    pipeline_viewport_state_create_info as pvs, render_pass_create_info as rpc,
    shader_module_create_info as smc, specialization_info as si, specialization_map_entry as sme,
    stencil_op_state as sos, subpass_description as sd, vertex_input_attribute_description as via,
    vertex_input_binding_description as vib,
};
use crate::mem::Mem;
use crate::result::VkResult;

use super::{array, handles, rects, viewports};

/// A `VkRenderPassCreateInfo`: the description from subpass 0, and what the front end refuses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RenderPassInfo {
    /// The attachments and subpass 0.
    pub desc: RenderPassDesc,
    /// `subpassCount`.
    pub subpasses: u32,
    /// The input attachments of subpass 0.
    pub input_attachments: u32,
}

fn attachment_index(m: &impl Mem, at: u64) -> VkResult<Option<u32>> {
    let a = m.u32(at + ar::ATTACHMENT)?;
    Ok((a != ATTACHMENT_UNUSED).then_some(a))
}

/// Reads a `VkRenderPassCreateInfo`.
pub fn render_pass(m: &impl Mem, at: u64) -> VkResult<RenderPassInfo> {
    let attachments = array(
        m,
        m.u64(at + rpc::P_ATTACHMENTS)?,
        m.u32(at + rpc::ATTACHMENT_COUNT)?,
        ad::SIZE,
        |m, a| {
            Ok(AttachmentDesc {
                format: m.u32(a + ad::FORMAT)?,
                samples: m.u32(a + ad::SAMPLES)?.max(1),
                load: m.u32(a + ad::LOAD_OP)?,
                store: m.u32(a + ad::STORE_OP)?,
                stencil_load: m.u32(a + ad::STENCIL_LOAD_OP)?,
                stencil_store: m.u32(a + ad::STENCIL_STORE_OP)?,
            })
        },
    )?;
    let subpasses = m.u32(at + rpc::SUBPASS_COUNT)?;
    let sp = m.u64(at + rpc::P_SUBPASSES)?;
    let mut info = RenderPassInfo {
        desc: RenderPassDesc {
            attachments,
            ..RenderPassDesc::default()
        },
        subpasses,
        input_attachments: 0,
    };
    if subpasses == 0 || sp == 0 {
        return Ok(info);
    }
    let n = m.u32(sp + sd::COLOR_ATTACHMENT_COUNT)?;
    info.desc.colors = array(
        m,
        m.u64(sp + sd::P_COLOR_ATTACHMENTS)?,
        n,
        ar::SIZE,
        |m, a| attachment_index(m, a),
    )?;
    info.desc.resolves = array(
        m,
        m.u64(sp + sd::P_RESOLVE_ATTACHMENTS)?,
        n,
        ar::SIZE,
        |m, a| attachment_index(m, a),
    )?;
    let ds = m.u64(sp + sd::P_DEPTH_STENCIL_ATTACHMENT)?;
    info.desc.depth = if ds == 0 {
        None
    } else {
        attachment_index(m, ds)?
    };
    info.input_attachments = m.u32(sp + sd::INPUT_ATTACHMENT_COUNT)?;
    Ok(info)
}

/// A `VkFramebufferCreateInfo`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FramebufferInfo {
    /// The image views.
    pub views: Vec<u64>,
    /// `width`.
    pub width: u32,
    /// `height`.
    pub height: u32,
    /// `layers`.
    pub layers: u32,
}

/// Reads a `VkFramebufferCreateInfo`.
pub fn framebuffer(m: &impl Mem, at: u64) -> VkResult<FramebufferInfo> {
    Ok(FramebufferInfo {
        views: handles(
            m,
            m.u64(at + fbc::P_ATTACHMENTS)?,
            m.u32(at + fbc::ATTACHMENT_COUNT)?,
        )?,
        width: m.u32(at + fbc::WIDTH)?,
        height: m.u32(at + fbc::HEIGHT)?,
        layers: m.u32(at + fbc::LAYERS)?,
    })
}

/// Reads the SPIR-V of a `VkShaderModuleCreateInfo`.
pub fn shader_code(m: &impl Mem, at: u64) -> VkResult<Vec<u8>> {
    m.bytes(m.u64(at + smc::P_CODE)?, m.u64(at + smc::CODE_SIZE)?)
}

/// One `VkPipelineShaderStageCreateInfo`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StageInfo {
    /// `stage`.
    pub stage: u32,
    /// `module`.
    pub module: u64,
    /// `pName`.
    pub entry: String,
    /// The specialization constants: id and value bytes.
    pub specialization: Vec<(u32, Vec<u8>)>,
}

fn stage(m: &impl Mem, at: u64) -> VkResult<StageInfo> {
    let spec = m.u64(at + pss::P_SPECIALIZATION_INFO)?;
    let mut specialization = Vec::new();
    if spec != 0 {
        let data = m.bytes(m.u64(spec + si::P_DATA)?, m.u64(spec + si::DATA_SIZE)?)?;
        let n = m.u32(spec + si::MAP_ENTRY_COUNT)?;
        specialization = array(m, m.u64(spec + si::P_MAP_ENTRIES)?, n, sme::SIZE, |m, e| {
            let off = m.u32(e + sme::OFFSET)? as usize;
            let len = m.u64(e + sme::SIZE_)? as usize;
            let bytes = data
                .get(off..off.saturating_add(len))
                .unwrap_or(&[])
                .to_vec();
            Ok((m.u32(e + sme::CONSTANT_ID)?, bytes))
        })?;
    }
    Ok(StageInfo {
        stage: m.u32(at + pss::STAGE)?,
        module: m.u64(at + pss::MODULE)?,
        entry: m.cstr(m.u64(at + pss::P_NAME)?, 256)?,
        specialization,
    })
}

/// A `VkGraphicsPipelineCreateInfo`, with the objects it names still as handles.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GraphicsPipelineInfo {
    /// The stages.
    pub stages: Vec<StageInfo>,
    /// The fixed-function state.
    pub state: PipelineState,
    /// `layout`.
    pub layout: u64,
    /// `renderPass`.
    pub render_pass: u64,
    /// `subpass`.
    pub subpass: u32,
}

fn stencil_face(m: &impl Mem, at: u64) -> VkResult<StencilFace> {
    Ok(StencilFace {
        fail: m.u32(at + sos::FAIL_OP)?,
        pass: m.u32(at + sos::PASS_OP)?,
        depth_fail: m.u32(at + sos::DEPTH_FAIL_OP)?,
        compare: m.u32(at + sos::COMPARE_OP)?,
        compare_mask: m.u32(at + sos::COMPARE_MASK)?,
        write_mask: m.u32(at + sos::WRITE_MASK)?,
        reference: m.u32(at + sos::REFERENCE)?,
    })
}

fn vertex_input(m: &impl Mem, vi: u64, s: &mut PipelineState) -> VkResult<()> {
    if vi == 0 {
        return Ok(());
    }
    let nb = m.u32(vi + pvi::VERTEX_BINDING_DESCRIPTION_COUNT)?;
    s.bindings = array(
        m,
        m.u64(vi + pvi::P_VERTEX_BINDING_DESCRIPTIONS)?,
        nb,
        vib::SIZE,
        |m, b| {
            Ok(VertexBinding {
                binding: m.u32(b + vib::BINDING)?,
                stride: m.u32(b + vib::STRIDE)?,
                per_instance: m.u32(b + vib::INPUT_RATE)? == 1,
            })
        },
    )?;
    let na = m.u32(vi + pvi::VERTEX_ATTRIBUTE_DESCRIPTION_COUNT)?;
    s.attributes = array(
        m,
        m.u64(vi + pvi::P_VERTEX_ATTRIBUTE_DESCRIPTIONS)?,
        na,
        via::SIZE,
        |m, a| {
            let format = m.u32(a + via::FORMAT)?;
            Ok(VertexAttribute {
                location: m.u32(a + via::LOCATION)?,
                binding: m.u32(a + via::BINDING)?,
                format,
                vertex: format::lookup(format).and_then(|f| f.vertex),
                offset: m.u32(a + via::OFFSET)?,
            })
        },
    )?;
    Ok(())
}

fn raster(m: &impl Mem, rs: u64) -> VkResult<Raster> {
    if rs == 0 {
        return Ok(Raster {
            line_width: 1.0,
            ..Raster::default()
        });
    }
    let bias = m.bool32(rs + prs::DEPTH_BIAS_ENABLE)?;
    Ok(Raster {
        depth_clamp: m.bool32(rs + prs::DEPTH_CLAMP_ENABLE)?,
        discard: m.bool32(rs + prs::RASTERIZER_DISCARD_ENABLE)?,
        polygon_mode: m.u32(rs + prs::POLYGON_MODE)?,
        cull_mode: m.u32(rs + prs::CULL_MODE)?,
        front_face: m.u32(rs + prs::FRONT_FACE)?,
        depth_bias: if bias {
            Some([
                m.f32(rs + prs::DEPTH_BIAS_CONSTANT_FACTOR)?,
                m.f32(rs + prs::DEPTH_BIAS_CLAMP)?,
                m.f32(rs + prs::DEPTH_BIAS_SLOPE_FACTOR)?,
            ])
        } else {
            None
        },
        line_width: m.f32(rs + prs::LINE_WIDTH)?,
    })
}

fn blend(m: &impl Mem, cb: u64, s: &mut PipelineState) -> VkResult<()> {
    if cb == 0 {
        return Ok(());
    }
    if m.bool32(cb + pcb::LOGIC_OP_ENABLE)? {
        let op = m.u32(cb + pcb::LOGIC_OP)?;
        // VK_LOGIC_OP_COPY (3) is what no logic op does.
        s.logic_op = (op != 3).then_some(op);
    }
    let n = m.u32(cb + pcb::ATTACHMENT_COUNT)?;
    s.blend = array(m, m.u64(cb + pcb::P_ATTACHMENTS)?, n, pcba::SIZE, |m, b| {
        Ok(BlendAttachment {
            enable: m.bool32(b + pcba::BLEND_ENABLE)?,
            src_color: m.u32(b + pcba::SRC_COLOR_BLEND_FACTOR)?,
            dst_color: m.u32(b + pcba::DST_COLOR_BLEND_FACTOR)?,
            color_op: m.u32(b + pcba::COLOR_BLEND_OP)?,
            src_alpha: m.u32(b + pcba::SRC_ALPHA_BLEND_FACTOR)?,
            dst_alpha: m.u32(b + pcba::DST_ALPHA_BLEND_FACTOR)?,
            alpha_op: m.u32(b + pcba::ALPHA_BLEND_OP)?,
            write_mask: m.u32(b + pcba::COLOR_WRITE_MASK)?,
        })
    })?;
    for (i, c) in s.blend_constants.iter_mut().enumerate() {
        *c = m.f32(cb + pcb::BLEND_CONSTANTS + 4 * i as u64)?;
    }
    Ok(())
}

/// Reads one `VkGraphicsPipelineCreateInfo`.
pub fn graphics_pipeline(m: &impl Mem, at: u64) -> VkResult<GraphicsPipelineInfo> {
    let mut s = PipelineState {
        topology: 3,
        viewport_count: 1,
        samples: 1,
        ..PipelineState::default()
    };
    let stages = array(
        m,
        m.u64(at + gpc::P_STAGES)?,
        m.u32(at + gpc::STAGE_COUNT)?,
        pss::SIZE,
        |m, a| stage(m, a),
    )?;
    vertex_input(m, m.u64(at + gpc::P_VERTEX_INPUT_STATE)?, &mut s)?;
    let ia = m.u64(at + gpc::P_INPUT_ASSEMBLY_STATE)?;
    if ia != 0 {
        s.topology = m.u32(ia + pia::TOPOLOGY)?;
        s.primitive_restart = m.bool32(ia + pia::PRIMITIVE_RESTART_ENABLE)?;
    }
    let dy = m.u64(at + gpc::P_DYNAMIC_STATE)?;
    if dy != 0 {
        s.dynamic = m.u32s(
            m.u64(dy + pdy::P_DYNAMIC_STATES)?,
            m.u32(dy + pdy::DYNAMIC_STATE_COUNT)?,
        )?;
    }
    let vs = m.u64(at + gpc::P_VIEWPORT_STATE)?;
    if vs != 0 {
        s.viewport_count = m.u32(vs + pvs::VIEWPORT_COUNT)?.max(1);
        if !s.is_dynamic(crate::consts::dynamic_state::VIEWPORT) {
            s.viewports = viewports(
                m,
                m.u64(vs + pvs::P_VIEWPORTS)?,
                m.u32(vs + pvs::VIEWPORT_COUNT)?,
            )?;
        }
        if !s.is_dynamic(crate::consts::dynamic_state::SCISSOR) {
            s.scissors = rects(
                m,
                m.u64(vs + pvs::P_SCISSORS)?,
                m.u32(vs + pvs::SCISSOR_COUNT)?,
            )?;
        }
    }
    s.raster = raster(m, m.u64(at + gpc::P_RASTERIZATION_STATE)?)?;
    let ms = m.u64(at + gpc::P_MULTISAMPLE_STATE)?;
    if ms != 0 {
        s.samples = m.u32(ms + pms::RASTERIZATION_SAMPLES)?.max(1);
        s.alpha_to_coverage = m.bool32(ms + pms::ALPHA_TO_COVERAGE_ENABLE)?;
    }
    let ds = m.u64(at + gpc::P_DEPTH_STENCIL_STATE)?;
    if ds != 0 {
        s.depth_stencil = Some(DepthStencil {
            depth_test: m.bool32(ds + pds::DEPTH_TEST_ENABLE)?,
            depth_write: m.bool32(ds + pds::DEPTH_WRITE_ENABLE)?,
            depth_compare: m.u32(ds + pds::DEPTH_COMPARE_OP)?,
            bounds_test: m.bool32(ds + pds::DEPTH_BOUNDS_TEST_ENABLE)?,
            stencil_test: m.bool32(ds + pds::STENCIL_TEST_ENABLE)?,
            front: stencil_face(m, ds + pds::FRONT)?,
            back: stencil_face(m, ds + pds::BACK)?,
        });
    }
    blend(m, m.u64(at + gpc::P_COLOR_BLEND_STATE)?, &mut s)?;
    Ok(GraphicsPipelineInfo {
        stages,
        state: s,
        layout: m.u64(at + gpc::LAYOUT)?,
        render_pass: m.u64(at + gpc::RENDER_PASS)?,
        subpass: m.u32(at + gpc::SUBPASS)?,
    })
}

/// Reads `n` `VkGraphicsPipelineCreateInfo`.
pub fn graphics_pipelines(m: &impl Mem, at: u64, n: u32) -> VkResult<Vec<GraphicsPipelineInfo>> {
    array(m, at, n, gpc::SIZE, |m, a| graphics_pipeline(m, a))
}

/// Reads `n` `VkComputePipelineCreateInfo`: the stage and the layout of each.
pub fn compute_pipelines(m: &impl Mem, at: u64, n: u32) -> VkResult<Vec<(StageInfo, u64)>> {
    array(m, at, n, cpc::SIZE, |m, a| {
        Ok((stage(m, a + cpc::STAGE)?, m.u64(a + cpc::LAYOUT)?))
    })
}
