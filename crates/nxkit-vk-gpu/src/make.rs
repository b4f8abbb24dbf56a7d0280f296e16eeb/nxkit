//! Making the Metal objects from the neutral descriptions of `nxkit-vk` (the reference layer's `vkCreateImage`,
//! `vkBindImageMemory`, `vkCreateImageView`, `vkCreateSampler`, `pipe_make` and `cpipe_make`).

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex};

use nxkit_abi::vk_table::Stage;
use nxkit_metal::{
    Context, DepthStencilState, Format, GuestMemory, PipelineCache, RenderPipelineDesc,
    StepFunction, Storage, Texture, TextureDesc, VertexAttribute, VertexDescriptor, VertexLayout,
};
use nxkit_vk::desc::{ComputePipelineDesc, GraphicsPipelineDesc, ImageDesc, ViewDesc};
use nxkit_vk::format::Conversion;

use crate::fixed::{self, Masks};
use crate::formats;
use crate::objects::{Compute, Converted, Graphics, Tex};
use crate::table::{ShaderTable, StageKind};
use crate::vertex::{self, Step};

/// `VkImageUsageFlagBits`.
mod usage {
    pub(super) const TRANSFER_DST: u32 = 0x2;
    pub(super) const STORAGE: u32 = 0x8;
    pub(super) const COLOR_ATTACHMENT: u32 = 0x10;
    pub(super) const DEPTH_STENCIL_ATTACHMENT: u32 = 0x20;
    pub(super) const INPUT_ATTACHMENT: u32 = 0x80;
}

/// `VkShaderStageFlagBits`.
pub(crate) mod stage {
    /// `VK_SHADER_STAGE_VERTEX_BIT`.
    pub(crate) const VERTEX: u32 = 0x1;
    /// `VK_SHADER_STAGE_FRAGMENT_BIT`.
    pub(crate) const FRAGMENT: u32 = 0x10;
    /// `VK_SHADER_STAGE_COMPUTE_BIT`.
    pub(crate) const COMPUTE: u32 = 0x20;
}

/// `VkDynamicState`.
mod dynamic {
    pub(super) const VIEWPORT: u32 = 0;
    pub(super) const SCISSOR: u32 = 1;
    pub(super) const DEPTH_BIAS: u32 = 3;
    pub(super) const BLEND_CONSTANTS: u32 = 4;
    pub(super) const STENCIL_COMPARE_MASK: u32 = 6;
    pub(super) const STENCIL_WRITE_MASK: u32 = 7;
    pub(super) const STENCIL_REFERENCE: u32 = 8;
}

fn metal_format(pixel: nxkit_vk::format::PixelFormat, what: &str) -> Result<Format, String> {
    formats::pixel(pixel)
        .ok_or_else(|| format!("{what}: pixel format {pixel:?} is not implemented"))
}

/// The texture of an image. A linear image lies over `memory` (or, for a converted format, gets a texture of its own
/// that the submits fill from it).
pub(crate) fn image(
    ctx: &Context,
    d: &ImageDesc,
    memory: Option<Arc<GuestMemory>>,
) -> Result<Tex, String> {
    let format = metal_format(d.pixel, "image")?;
    let [w, h, depth] = d.extent.map(|v| v.max(1));
    if let Some(lin) = d.linear {
        let memory = memory.ok_or("the memory of the linear image has no storage")?;
        let mut td = TextureDesc::compute_2d(w, h, format);
        td.shader_write = d.usage & usage::STORAGE != 0;
        td.render_target = d.usage & usage::COLOR_ATTACHMENT != 0;
        if d.conversion != Conversion::None {
            td.shader_write = false;
            td.render_target = false;
            let texture = Texture::new(ctx, td).map_err(|e| e.to_string())?;
            let len = (lin.row_pitch * u64::from(h)) as usize;
            let converted = Converted {
                memory,
                offset: lin.offset,
                pitch: lin.row_pitch,
                conversion: d.conversion,
                // differs from anything the game wrote, so the first submit unpacks it
                shadow: vec![0xA5; len],
            };
            return Ok(Tex {
                texture,
                _memory: None,
                converted: Some(Mutex::new(converted)),
            });
        }
        let texture = Texture::over_guest(ctx, &memory, lin.offset, lin.row_pitch, td).map_err(|e| {
            format!(
                "the linear image {w}x{h} at offset {} with pitch {} cannot lie over its memory: {e:?}",
                lin.offset, lin.row_pitch
            )
        })?;
        return Ok(Tex {
            texture,
            _memory: Some(memory),
            converted: None,
        });
    }
    if d.samples > 1 {
        return Err(format!(
            "multisample images ({} samples) are not implemented",
            d.samples
        ));
    }
    let (kind, array_layers) = formats::kind(d.kind, d.layers)
        .ok_or_else(|| format!("images of kind {:?} are not implemented", d.kind))?;
    let plain = !format.is_compressed() && !format.is_depth() && !format.has_stencil();
    let attachment = usage::COLOR_ATTACHMENT
        | usage::DEPTH_STENCIL_ATTACHMENT
        | usage::INPUT_ATTACHMENT
        | usage::TRANSFER_DST;
    let td = TextureDesc {
        kind,
        width: w,
        height: h,
        depth: if kind == nxkit_metal::TextureKind::D3 {
            depth
        } else {
            1
        },
        mip_levels: d.levels.max(1),
        array_layers,
        allow_views: d.format_views,
        format,
        shader_read: true,
        shader_write: plain && d.usage & usage::STORAGE != 0,
        // a transfer destination may be cleared, which is a render pass; a compressed format draws no pass
        render_target: !format.is_compressed() && (d.usage & attachment != 0 || d.swapchain),
        storage: Storage::Private,
        shader_atomic: false,
    };
    let texture = Texture::new(ctx, td).map_err(|e| {
        format!(
            "cannot make image {w}x{h}x{depth} format {} levels {} layers {}: {e}",
            d.format, d.levels, d.layers
        )
    })?;
    Ok(Tex {
        texture,
        _memory: None,
        converted: None,
    })
}

/// The texture of a view of `image`.
pub(crate) fn view(image: &Tex, d: &ViewDesc) -> Result<Tex, String> {
    if d.whole {
        return Ok(Tex {
            texture: image.texture.clone(),
            _memory: None,
            converted: None,
        });
    }
    let format = metal_format(d.pixel, "image view")?;
    let (kind, _) = formats::kind(d.kind, d.layers)
        .ok_or_else(|| format!("views of kind {:?} are not implemented", d.kind))?;
    let (first_slice, slice_count) = formats::view_slices(kind, d.base_layer, d.layers);
    let v = nxkit_metal::ViewDesc {
        format,
        kind,
        first_level: d.base_level,
        level_count: d.levels.max(1),
        first_slice,
        slice_count,
    };
    let texture = image
        .texture
        .view_swizzled(&v, formats::swizzle(d.swizzle))
        .map_err(|e| {
            format!(
                "cannot make an image view of format {} as {v:?}: {e}",
                d.format
            )
        })?;
    Ok(Tex {
        texture,
        _memory: None,
        converted: None,
    })
}

/// The Metal vertex descriptor of a vertex plan.
fn vertex_descriptor(p: &vertex::VertexPlan) -> Result<VertexDescriptor, String> {
    let mut d = VertexDescriptor::default();
    for a in &p.attributes {
        let format = formats::vertex(a.format)
            .ok_or_else(|| format!("vertex format {:?} is not implemented", a.format))?;
        d.attributes.push((
            a.location,
            VertexAttribute {
                format,
                offset: a.offset,
                buffer: a.slot,
            },
        ));
    }
    for l in &p.layouts {
        d.layouts.push(VertexLayout {
            buffer: l.slot,
            stride: l.stride,
            step: match l.step {
                Step::Vertex => StepFunction::PerVertex,
                Step::Instance => StepFunction::PerInstance,
                Step::Constant => StepFunction::Constant,
            },
            step_rate: 1,
        });
    }
    Ok(d)
}

fn key(v: &impl Hash) -> u64 {
    let mut h = DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

/// The key of a render pipeline in the cache: everything that Metal builds it from. Pipelines of the same state share
/// one state, and a pipeline that the game makes again after a destroy is not compiled again.
fn render_key(rd: &RenderPipelineDesc<'_>) -> u64 {
    key(&(
        "render",
        rd.vertex.1,
        rd.fragment.map(|f| f.1),
        &rd.colors,
        &rd.vertex_layout,
        rd.topology,
        rd.sample_count,
        rd.alpha_to_coverage,
    ))
}

/// What a graphics pipeline build reports beside its result: the parts that Metal cannot do and that the pipeline
/// runs without.
pub(crate) type Lacking = Vec<String>;

/// Builds a graphics pipeline.
pub(crate) fn graphics(
    ctx: &Context,
    shaders: &ShaderTable,
    cache: &PipelineCache,
    d: &GraphicsPipelineDesc,
) -> Result<(Graphics, Lacking), String> {
    let mut lacking: Lacking = Vec::new();
    let (mut vs, mut fs) = (None, None);
    for s in &d.stages {
        match s.stage {
            stage::VERTEX => vs = Some(shaders.shader(s.key, StageKind::Vertex)?),
            stage::FRAGMENT => fs = Some(shaders.shader(s.key, StageKind::Fragment)?),
            other => return Err(format!("shader stage {other:#x} is not implemented")),
        }
        if !s.specialization.is_empty() {
            lacking.push(format!(
                "specialization constants of shader {} (the build translated their defaults)",
                s.key.to_hex()
            ));
        }
    }
    let name = d
        .stages
        .iter()
        .map(|s| s.key.to_hex())
        .collect::<Vec<_>>()
        .join(" ");
    let vs = vs.ok_or("the pipeline has no vertex shader")?;
    let library = shaders.library().ok_or("the shader library did not load")?;
    let st = &d.state;
    let (primitive, class) = fixed::topology(st.topology)
        .ok_or_else(|| format!("primitive topology {} is not implemented", st.topology))?;
    if st.raster.discard {
        return Err("rasterizer discard is not implemented".into());
    }
    if st.viewport_count > 1 {
        lacking.push(format!(
            "{} viewports (the first one is used)",
            st.viewport_count
        ));
    }
    let plan = vertex::plan(&vs.row.resources, &st.bindings, &st.attributes)?;
    lacking.extend(plan.skipped.iter().cloned());
    let fragment_fn = fs.as_ref().map(|f| match (&f.dual, st.dual_source()) {
        (Some(dual), true) => dual.clone(),
        (None, true) => {
            lacking.push(format!("the dual-source variant of {}", f.function));
            f.function.clone()
        }
        _ => f.function.clone(),
    });
    let colors = d
        .targets
        .colors
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let vk = (*c)?;
            let format = formats::texture_format(vk)?;
            let blend = if st.blend.len() > 1 {
                st.blend.get(i)
            } else {
                st.blend.first()
            };
            Some(fixed::color_target(format, vk, blend))
        })
        .collect();
    let mut rd = RenderPipelineDesc::new((library, &vs.function), None, &[]);
    rd.fragment = fragment_fn.as_deref().map(|f| (library, f));
    rd.colors = colors;
    rd.vertex_layout = vertex_descriptor(&plan)?;
    rd.topology = class;
    rd.sample_count = st.samples.max(1);
    rd.alpha_to_coverage = st.alpha_to_coverage;
    let pipeline = cache
        .render(render_key(&rd), &rd)
        .map_err(|e| format!("cannot build the pipeline ({name}): {e}"))?;
    let ds = st.depth_stencil.as_ref();
    let ds_desc = fixed::depth_stencil(ds, d.targets.depth, None);
    let depth_stencil =
        DepthStencilState::new(ctx, &ds_desc).map_err(|e| format!("depth stencil state: {e}"))?;
    let masks_dynamic =
        st.is_dynamic(dynamic::STENCIL_COMPARE_MASK) || st.is_dynamic(dynamic::STENCIL_WRITE_MASK);
    let dynamic_masks = match ds {
        Some(ds) if masks_dynamic && ds_desc.front.is_some() => Some((ds_desc, Masks::of(ds))),
        _ => None,
    };
    if ds.is_some_and(|ds| ds.bounds_test) {
        lacking.push("the depth bounds test".into());
    }
    let (raster, raster_lacking) = fixed::raster(&st.raster);
    lacking.extend(raster_lacking.into_iter().map(String::from));
    let stencil_ref = ds.map_or((0, 0), |ds| (ds.front.reference, ds.back.reference));
    let g = Graphics {
        pipeline,
        depth_stencil,
        dynamic_masks,
        mask_states: Mutex::new(HashMap::new()),
        primitive,
        cull: raster.cull,
        winding: raster.winding,
        fill: raster.fill,
        clip: raster.clip,
        bias: raster.bias,
        blend_constants: st.blend_constants,
        stencil_ref,
        viewport: (!st.is_dynamic(dynamic::VIEWPORT))
            .then(|| st.viewports.first().copied())
            .flatten(),
        scissor: (!st.is_dynamic(dynamic::SCISSOR))
            .then(|| st.scissors.first().copied())
            .flatten(),
        dynamic_bias: st.is_dynamic(dynamic::DEPTH_BIAS),
        dynamic_blend: st.is_dynamic(dynamic::BLEND_CONSTANTS),
        dynamic_ref: st.is_dynamic(dynamic::STENCIL_REFERENCE),
        vertex: plan,
        resources: [
            Arc::from(vs.row.resources.as_slice()),
            fs.map_or_else(
                || Arc::from(Vec::new()),
                |f| Arc::from(f.row.resources.as_slice()),
            ),
        ],
        push_size: d.layout.push_size,
        logic_op: st.logic_op,
        name,
    };
    Ok((g, lacking))
}

/// Builds a compute pipeline. It uses the shader's full local size.
pub(crate) fn compute(
    shaders: &ShaderTable,
    cache: &PipelineCache,
    d: &ComputePipelineDesc,
) -> Result<Compute, String> {
    let s = shaders.shader(d.stage.key, StageKind::Compute)?;
    let Stage::Comp(threads) = s.row.stage else {
        return Err(format!(
            "shader {} is not a compute shader",
            d.stage.key.to_hex()
        ));
    };
    let library = shaders.library().ok_or("the shader library did not load")?;
    let pipeline = cache
        .compute(key(&("compute", &s.function)), library, &s.function)
        .map_err(|e| {
            format!(
                "cannot build the compute pipeline {}: {e}",
                d.stage.key.to_hex()
            )
        })?;
    Ok(Compute {
        pipeline,
        threads,
        resources: Arc::from(s.row.resources.as_slice()),
        push_size: d.layout.push_size,
    })
}
