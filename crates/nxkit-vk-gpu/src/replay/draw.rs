//! A render segment and its draws (the reference layer's `begin_rp`, `pre_draw`, `apply_viewports` and the draw
//! commands).

use std::sync::Arc;

use nxkit_metal::{CommandBuffer, RenderCommand, RenderPass};
use nxkit_vk::cmd::{ClearAttachment, Cmd, IndexType};
use nxkit_vk::desc::Viewport;

use super::attach::desc;
use super::{Replay, State};
use crate::bind::{self, Bound};
use crate::fixed::{self, Masks};
use crate::objects::Graphics;
use crate::passplan::{self, Start};

/// What the draws of one segment have set.
#[derive(Default)]
struct Applied {
    pipeline: Option<nxkit_vk::desc::PipelineId>,
    viewport: Option<Viewport>,
    scissor: Option<nxkit_metal::Scissor>,
}

/// Runs a render segment from `cmds[i]`: `resume` None for the begin of the pass, the clears of the resume otherwise.
/// Gives the index of the first command after it.
pub(super) fn segment(
    r: &mut Replay<'_>,
    st: &mut State,
    cb: &mut CommandBuffer,
    cmds: &[&Cmd],
    i: usize,
    resume: Option<&[ClearAttachment]>,
) -> usize {
    let Some(pass) = st.pass.as_mut() else {
        return i;
    };
    if pass.broken {
        return i;
    }
    pass.cut = false;
    let begin = pass.begin.clone();
    let start = match resume {
        None => Start::Begin(&begin.clear),
        Some(c) => Start::Resume(c),
    };
    let actions = passplan::actions(&begin.pass, &start, passplan::segment_end(cmds, i));
    let d = desc(
        &pass.colors,
        pass.depth.as_ref(),
        &begin.pass.color_formats(),
        &actions,
    );
    let size = pass.size;
    let mut rp = match cb.begin_render_pass_split(&d, &r.tables.vertex, &r.tables.fragment) {
        Ok(rp) => rp,
        Err(e) => {
            r.log.once(
                "pass",
                format_args!("[RED] a render pass cannot begin: {e}"),
            );
            pass.broken = true;
            return i;
        }
    };
    r.stats.passes += 1;
    let _ = rp.encode(&RenderCommand::WaitForEarlierPasses);
    let mut applied = Applied::default();
    let mut j = i;
    while let Some(&c) = cmds.get(j) {
        match c {
            Cmd::EndRenderPass => {
                rp.end();
                st.pass = None;
                return j + 1;
            }
            Cmd::BeginRenderPass(_) => {
                rp.end();
                st.pass = None;
                return j;
            }
            Cmd::ClearAttachments { attachments, rects } => {
                rp.end();
                if let Some(p) = st.pass.as_mut() {
                    p.cut = true;
                    if passplan::covers(rects, size.0, size.1) {
                        p.clears = attachments.clone();
                    } else {
                        r.log.once(
                            "clear part",
                            "[RED] vkCmdClearAttachments of a part of an attachment is not implemented",
                        );
                    }
                }
                return j + 1;
            }
            c if passplan::is_cut(c) => {
                rp.end();
                if let Some(p) = st.pass.as_mut() {
                    p.cut = true;
                }
                return j;
            }
            c if c.is_draw() => draw(r, st, &mut rp, &mut applied, size, c),
            c => {
                if !st.apply(c, r.sets) {
                    r.log.once(
                        format!("cmd {}", c.name()),
                        format_args!("[RED] {} inside a render pass is not implemented", c.name()),
                    );
                }
            }
        }
        j += 1;
    }
    rp.end();
    if let Some(p) = st.pass.as_mut() {
        p.cut = true;
    }
    j
}

/// Sets the state of a pipeline, its dynamic state, the viewport and the scissor.
fn pipeline_state(
    r: &Replay<'_>,
    st: &State,
    rp: &mut RenderPass<'_>,
    applied: &mut Applied,
    g: &Graphics,
    id: nxkit_vk::desc::PipelineId,
    size: (u32, u32),
) -> nxkit_metal::Result<()> {
    if applied.pipeline != Some(id) {
        rp.encode(&RenderCommand::SetPipeline(g.pipeline.clone()))?;
        rp.encode(&RenderCommand::SetCullMode(g.cull))?;
        rp.encode(&RenderCommand::SetWinding(g.winding))?;
        rp.encode(&RenderCommand::SetFillMode(g.fill))?;
        rp.encode(&RenderCommand::SetDepthClip(g.clip))?;
        applied.pipeline = Some(id);
    }
    let bias = match (g.bias, st.bias) {
        (Some(_), Some(b)) if g.dynamic_bias => b,
        (Some(b), _) => b,
        (None, _) => [0.0; 3],
    };
    rp.encode(&RenderCommand::SetDepthBias(nxkit_metal::DepthBias {
        constant: bias[0],
        clamp: bias[1],
        slope_scale: bias[2],
    }))?;
    let blend = match st.blend {
        Some(b) if g.dynamic_blend => b,
        _ => g.blend_constants,
    };
    rp.encode(&RenderCommand::SetBlendColor(blend))?;
    let (front, back) = match g.dynamic_ref {
        true => (
            st.stencil[2][0].unwrap_or(g.stencil_ref.0),
            st.stencil[2][1].unwrap_or(g.stencil_ref.1),
        ),
        false => g.stencil_ref,
    };
    rp.encode(&RenderCommand::SetStencilRef { front, back })?;
    let ds = match &g.dynamic_masks {
        Some((desc, base)) => {
            let m = Masks {
                front_compare: st.stencil[0][0].unwrap_or(base.front_compare),
                back_compare: st.stencil[0][1].unwrap_or(base.back_compare),
                front_write: st.stencil[1][0].unwrap_or(base.front_write),
                back_write: st.stencil[1][1].unwrap_or(base.back_write),
            };
            mask_state(r, g, desc, m)
        }
        None => g.depth_stencil.clone(),
    };
    rp.encode(&RenderCommand::SetDepthStencil(ds))?;
    let full = Viewport {
        x: 0.0,
        y: 0.0,
        width: size.0 as f32,
        height: size.1 as f32,
        min_depth: 0.0,
        max_depth: 1.0,
    };
    let v = g.viewport.or(st.viewport).unwrap_or(full);
    if applied.viewport != Some(v) {
        rp.encode(&RenderCommand::SetViewport(nxkit_metal::Viewport {
            x: f64::from(v.x),
            y: f64::from(v.y),
            width: f64::from(v.width),
            height: f64::from(v.height),
            z_near: f64::from(v.min_depth),
            z_far: f64::from(v.max_depth),
        }))?;
        applied.viewport = Some(v);
    }
    let whole = nxkit_vk::desc::Rect2D {
        x: 0,
        y: 0,
        width: size.0,
        height: size.1,
    };
    let s = fixed::scissor(g.scissor.or(st.scissor).unwrap_or(whole), size.0, size.1);
    if applied.scissor != Some(s) {
        rp.encode(&RenderCommand::SetScissor(s))?;
        applied.scissor = Some(s);
    }
    Ok(())
}

/// The depth-stencil state of a pipeline with dynamic stencil masks.
fn mask_state(
    r: &Replay<'_>,
    g: &Graphics,
    desc: &nxkit_metal::DepthStencilDesc,
    m: Masks,
) -> nxkit_metal::DepthStencilState {
    let mut states = g
        .mask_states
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(s) = states.get(&m) {
        return s.clone();
    }
    let mut d = *desc;
    d.front = d.front.map(|f| nxkit_metal::StencilFace {
        read_mask: m.front_compare,
        write_mask: m.front_write,
        ..f
    });
    d.back = d.back.map(|f| nxkit_metal::StencilFace {
        read_mask: m.back_compare,
        write_mask: m.back_write,
        ..f
    });
    match nxkit_metal::DepthStencilState::new(r.ctx, &d) {
        Ok(s) => {
            states.insert(m, s.clone());
            s
        }
        Err(_) => g.depth_stencil.clone(),
    }
}

/// Binds the resources of both stages, the push constants and the vertex buffers.
fn bindings(
    r: &Replay<'_>,
    st: &State,
    rp: &mut RenderPass<'_>,
    g: &Graphics,
) -> Result<(), String> {
    let mut out = Vec::new();
    let mut problems = Vec::new();
    for (stage, resources) in g.resources.iter().enumerate() {
        out.clear();
        bind::resolve(
            resources,
            &st.sets[0],
            |h| r.sets.get(&h),
            &mut out,
            &mut problems,
        );
        let frag = stage == 1;
        for b in &out {
            let cmd = match *b {
                Bound::Buffer { slot, at } => {
                    let Some(gpu_addr) = r.objects.address(at.memory, at.offset) else {
                        problems.push(format!("the buffer at slot {slot} has no memory"));
                        continue;
                    };
                    match frag {
                        true => RenderCommand::SetFragmentBuffer {
                            index: slot,
                            gpu_addr,
                        },
                        false => RenderCommand::SetBuffer {
                            index: slot,
                            gpu_addr,
                        },
                    }
                }
                Bound::Texture { slot, id } => {
                    let Some(texture) = r.objects.texture(id).cloned() else {
                        problems.push(format!("the texture at slot {slot} was destroyed"));
                        continue;
                    };
                    match frag {
                        true => RenderCommand::SetFragmentTexture {
                            index: slot,
                            texture,
                        },
                        false => RenderCommand::SetTexture {
                            index: slot,
                            texture,
                        },
                    }
                }
                Bound::Sampler { slot, id } => {
                    let Some(sampler) = r.objects.samplers.get(&id).cloned() else {
                        problems.push(format!("the sampler at slot {slot} was destroyed"));
                        continue;
                    };
                    match frag {
                        true => RenderCommand::SetFragmentSampler {
                            index: slot,
                            sampler,
                        },
                        false => RenderCommand::SetSampler {
                            index: slot,
                            sampler,
                        },
                    }
                }
            };
            rp.encode(&cmd).map_err(|e| e.to_string())?;
        }
        if let Some(slot) = bind::push_slot(resources) {
            let n = bind::push_len(g.push_size);
            let data = st.push[stage].get(..n).unwrap_or_default();
            let gpu_addr = r.scratch.push(data).map_err(|e| e.to_string())?;
            let cmd = match frag {
                true => RenderCommand::SetFragmentBuffer {
                    index: slot,
                    gpu_addr,
                },
                false => RenderCommand::SetBuffer {
                    index: slot,
                    gpu_addr,
                },
            };
            rp.encode(&cmd).map_err(|e| e.to_string())?;
        }
    }
    for l in &g.vertex.layouts {
        let at = st
            .vertex_buffers
            .get(l.binding as usize)
            .copied()
            .flatten()
            .ok_or_else(|| format!("vertex buffer {} is not bound", l.binding))?;
        let gpu_addr = r
            .objects
            .address(at.memory, at.offset)
            .ok_or_else(|| format!("vertex buffer {} has no memory", l.binding))?;
        rp.encode(&RenderCommand::SetBuffer {
            index: l.slot,
            gpu_addr,
        })
        .map_err(|e| e.to_string())?;
    }
    if let Some(p) = problems.first() {
        r.log.once(
            format!("bind {} {p}", g.name),
            format_args!("[RED] pipeline {}: {p}", g.name),
        );
    }
    Ok(())
}

/// Encodes one draw.
fn draw(
    r: &mut Replay<'_>,
    st: &State,
    rp: &mut RenderPass<'_>,
    applied: &mut Applied,
    size: (u32, u32),
    c: &Cmd,
) {
    let Some(id) = st.graphics else {
        r.stats.skipped += 1;
        return;
    };
    let Some(g) = r.objects.graphics(id) else {
        r.stats.skipped += 1;
        r.log.once(
            format!("pipeline {}", id.0),
            format_args!("[RED] draws skipped: pipeline {:#x} was not built", id.0),
        );
        return;
    };
    if let Some(op) = g.logic_op {
        r.log.once(
            "logic op",
            format_args!("[RED] logic op {op} is not implemented: the pipeline draws with its shader's colour"),
        );
    }
    let ok = pipeline_state(r, st, rp, applied, &g, id, size)
        .map_err(|e| e.to_string())
        .and_then(|()| bindings(r, st, rp, &g))
        .and_then(|()| encode_draw(r, st, rp, &g, c));
    match ok {
        Ok(true) => r.stats.draws += 1,
        Ok(false) => r.stats.skipped += 1,
        Err(e) => {
            r.stats.skipped += 1;
            r.log.once(
                format!("draw {} {e}", g.name),
                format_args!("[RED] a draw of pipeline {} is skipped: {e}", g.name),
            );
        }
    }
}

/// The index buffer of an indexed draw: its address, its bytes and its index type. Uint8 indices are not drawn.
fn index_buffer(r: &Replay<'_>, st: &State) -> Result<(u64, u64, nxkit_metal::IndexType), String> {
    let (b, t) = st.index.ok_or("no index buffer is bound")?;
    let t = match t {
        IndexType::U16 => nxkit_metal::IndexType::U16,
        IndexType::U32 => nxkit_metal::IndexType::U32,
        IndexType::U8 => return Err("8-bit indices are not implemented".into()),
    };
    let addr = r
        .objects
        .address(b.memory, b.offset)
        .ok_or("the index buffer has no memory")?;
    Ok((addr, b.size, t))
}

/// Encodes the draw command itself. Gives false for an empty draw.
fn encode_draw(
    r: &Replay<'_>,
    st: &State,
    rp: &mut RenderPass<'_>,
    g: &Arc<Graphics>,
    c: &Cmd,
) -> Result<bool, String> {
    let primitive = g.primitive;
    let e =
        |rp: &mut RenderPass<'_>, cmd: RenderCommand| rp.encode(&cmd).map_err(|e| e.to_string());
    match *c {
        Cmd::Draw {
            vertices,
            instances,
            first_vertex,
            first_instance,
        } => {
            if vertices == 0 || instances == 0 {
                return Ok(false);
            }
            e(
                rp,
                RenderCommand::Draw {
                    primitive,
                    first_vertex,
                    vertex_count: vertices,
                    instance_count: instances,
                    base_instance: first_instance,
                },
            )?;
        }
        Cmd::DrawIndexed {
            indices,
            instances,
            first_index,
            vertex_offset,
            first_instance,
        } => {
            if indices == 0 || instances == 0 {
                return Ok(false);
            }
            let (addr, bytes, index_type) = index_buffer(r, st)?;
            let skip = u64::from(first_index) * index_type.bytes();
            e(
                rp,
                RenderCommand::DrawIndexed {
                    primitive,
                    index_type,
                    index_addr: addr + skip,
                    index_bytes: bytes.saturating_sub(skip),
                    index_count: indices,
                    instance_count: instances,
                    base_vertex: vertex_offset,
                    base_instance: first_instance,
                },
            )?;
        }
        Cmd::DrawIndirect {
            buffer,
            count,
            stride,
            indexed,
        } => {
            if count == 0 {
                return Ok(false);
            }
            let base = r
                .objects
                .address(buffer.memory, buffer.offset)
                .ok_or("the indirect buffer has no memory")?;
            let index = if indexed {
                Some(index_buffer(r, st)?)
            } else {
                None
            };
            for k in 0..u64::from(count) {
                let indirect_addr = base + k * u64::from(stride);
                let cmd = match index {
                    Some((index_addr, index_bytes, index_type)) => {
                        RenderCommand::DrawIndexedIndirect {
                            primitive,
                            index_type,
                            index_addr,
                            index_bytes,
                            indirect_addr,
                        }
                    }
                    None => RenderCommand::DrawIndirect {
                        primitive,
                        indirect_addr,
                    },
                };
                e(rp, cmd)?;
            }
        }
        _ => return Ok(false),
    }
    Ok(true)
}
