//! The encoding of draws and of replayed recordings.

use super::*;

/// What a GPU driven draw needs besides its [`Prep`]: the table of the object stage and the records that the pre-pass
/// of the render pass copied for it.
pub(super) struct Driven<'a> {
    object: &'a ArgumentTable,
    copies: Option<u64>,
}

pub(super) fn emit(
    rp: &mut RenderPass<'_>,
    tr: &mut Tracker,
    pc: &PassCtx,
    d: &DynamicDetail,
    p: &Prep,
    x: &Driven<'_>,
    stats: &mut EncodeStats,
) -> Result<(), MetalError> {
    if tr.pipeline != Some(p.key) {
        rp.encode(&RenderCommand::SetPipeline(p.pipeline.clone()))?;
        tr.pipeline = Some(p.key);
        stats.pipeline_switches += 1;
    }
    if tr.depth_stencil != Some(p.depth_stencil_desc) {
        rp.encode(&RenderCommand::SetDepthStencil(p.depth_stencil.clone()))?;
        tr.depth_stencil = Some(p.depth_stencil_desc);
    }
    if tr.winding != Some(p.winding) {
        rp.encode(&RenderCommand::SetWinding(p.winding))?;
        tr.winding = Some(p.winding);
    }
    if tr.fill != Some(p.fill) {
        rp.encode(&RenderCommand::SetFillMode(p.fill))?;
        tr.fill = Some(p.fill);
    }
    if tr.bias != Some(p.bias) {
        rp.encode(&RenderCommand::SetDepthBias(p.bias))?;
        tr.bias = Some(p.bias);
    }
    if tr.clip != Some(p.clip) {
        rp.encode(&RenderCommand::SetDepthClip(p.clip))?;
        tr.clip = Some(p.clip);
    }
    tr.dynamic(rp, pc, d, p.cull)?;
    if tr.vertex_args != Some(p.vertex_args) {
        rp.encode(&RenderCommand::SetBuffer {
            index: 0,
            gpu_addr: p.vertex_args,
        })?;
        tr.vertex_args = Some(p.vertex_args);
    }
    if let Some(a) = p.fragment_args.filter(|a| tr.fragment_args != Some(*a)) {
        rp.encode(&RenderCommand::SetFragmentBuffer {
            index: 0,
            gpu_addr: a,
        })?;
        tr.fragment_args = Some(a);
    }
    for (slot, addr) in &p.streams {
        let at = *slot as usize;
        if tr.streams.get(at).copied().flatten() != Some(*addr) {
            rp.encode(&RenderCommand::SetBuffer {
                index: *slot,
                gpu_addr: *addr,
            })?;
            if let Some(c) = tr.streams.get_mut(at) {
                *c = Some(*addr);
            }
        }
    }
    if let Some(a) = p.object_args.filter(|a| tr.object_args != Some(*a)) {
        if !rp.has_object_table() {
            rp.set_object_table(x.object);
        }
        rp.encode(&RenderCommand::SetObjectBuffer {
            index: 0,
            gpu_addr: a,
        })?;
        tr.object_args = Some(a);
    }
    let copies = || {
        x.copies.ok_or(MetalError::Invalid(
            "the pre-pass made no copy for the draw",
        ))
    };
    let first_slot = crate::mesh::MESH_FIRST_SLOT;
    // The first workgroup goes to the object stage when there is a task shader, else to the mesh stage. The slot of
    // the mesh stage is a vertex stream slot of the vertex stage, so the tracker forgets it.
    let first = |rp: &mut RenderPass<'_>, tr: &mut Tracker, task: bool, gpu_addr: u64| {
        if task {
            rp.encode(&RenderCommand::SetObjectBuffer {
                index: first_slot,
                gpu_addr,
            })
        } else {
            tr.streams[first_slot as usize] = None;
            rp.encode(&RenderCommand::SetBuffer {
                index: first_slot,
                gpu_addr,
            })
        }
    };
    match p.geom {
        Geom::Mesh {
            first: at,
            count,
            object_threads,
            mesh_threads,
            task,
        } => {
            let n = crate::mesh::splits(count, task);
            if !task {
                stats.kinds.clamped += count.saturating_sub(n * crate::mesh::MAX_GRID);
            }
            for k in 0..n {
                first(rp, tr, task, at + crate::mesh::FIRST_STRIDE * u64::from(k))?;
                let groups = if task {
                    count
                } else {
                    (count - k * crate::mesh::MAX_GRID).min(crate::mesh::MAX_GRID)
                };
                rp.encode(&RenderCommand::DrawMeshThreadgroups {
                    groups: [groups, 1, 1],
                    object_threads,
                    mesh_threads,
                })?;
            }
            stats.kinds.metal_draws += n;
            return Ok(());
        }
        Geom::MeshIndirect {
            splits,
            object_threads,
            mesh_threads,
            task,
        } => {
            let base = copies()?;
            for k in 0..u64::from(splits) {
                let rec = base + crate::indirect::RECORD * k;
                first(rp, tr, task, rec + 16)?;
                rp.encode(&RenderCommand::DrawMeshThreadgroupsIndirect {
                    indirect_addr: rec,
                    object_threads,
                    mesh_threads,
                })?;
            }
            stats.kinds.metal_draws += splits;
            return Ok(());
        }
        Geom::MultiIndirect {
            primitive,
            index,
            draws,
        } => {
            let base = copies()?;
            for i in 0..u64::from(draws) {
                let indirect_addr = base + crate::indirect::RECORD * i;
                rp.encode(&match index {
                    None => RenderCommand::DrawIndirect {
                        primitive,
                        indirect_addr,
                    },
                    Some((index_type, index_addr, index_bytes)) => {
                        RenderCommand::DrawIndexedIndirect {
                            primitive,
                            index_type,
                            index_addr,
                            index_bytes,
                            indirect_addr,
                        }
                    }
                })?;
            }
            stats.kinds.metal_draws += draws;
            return Ok(());
        }
        _ => {}
    }
    rp.encode(&match p.geom {
        Geom::Indirect {
            primitive,
            indirect,
        } => {
            stats.kinds.metal_draws += 1;
            RenderCommand::DrawIndirect {
                primitive,
                indirect_addr: indirect,
            }
        }
        Geom::IndexedIndirect {
            primitive,
            index_type,
            address,
            index_bytes,
            indirect,
        } => {
            stats.kinds.metal_draws += 1;
            RenderCommand::DrawIndexedIndirect {
                primitive,
                index_type,
                index_addr: address,
                index_bytes,
                indirect_addr: indirect,
            }
        }
        Geom::Mesh { .. } | Geom::MeshIndirect { .. } | Geom::MultiIndirect { .. } => return Ok(()),
        Geom::Arrays {
            primitive,
            first,
            count,
            instances,
            base_instance,
        } => RenderCommand::Draw {
            primitive,
            first_vertex: first,
            vertex_count: count,
            instance_count: instances,
            base_instance,
        },
        Geom::Indexed {
            primitive,
            index_type,
            address,
            count,
            instances,
            base_vertex,
            base_instance,
            ..
        } => RenderCommand::DrawIndexed {
            primitive,
            index_type,
            index_addr: address,
            index_bytes: u64::from(count) * index_type.bytes(),
            index_count: count,
            instance_count: instances,
            base_vertex,
            base_instance,
        },
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw(
    sh: &Shared<'_>,
    pc: &PassCtx,
    rp: &mut RenderPass<'_>,
    tr: &mut Tracker,
    scratch: &Scratch,
    memo: &mut ArgsMemo,
    pre: &Prepass,
    object: &ArgumentTable,
    n: u32,
    stats: &mut EncodeStats,
) {
    let Some(d) = sh.detail.draws.get(n as usize) else {
        return stats.skip(Skip::Detail);
    };
    static DRAW_SHOWN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if draw_diag("draw", &DRAW_SHOWN) {
        eprintln!(
            "DIAG draw {n} vs {} fs {:?} args {:?} depth {:?} raster {:?} masks {:?} blend {:?} inputs {:?} streams {:?} ubos {:?} textures {:?}",
            d.program.vertex,
            d.program.fragment,
            d.args,
            d.depth_stencil,
            d.raster,
            d.write_masks,
            d.blend,
            d.attributes,
            d.streams,
            d.ubos
                .iter()
                .map(|u| (
                    u.stage,
                    u.slot,
                    u.address,
                    u.size,
                    u.snapshot.as_ref().map(|s| s.len())
                ))
                .collect::<Vec<_>>(),
            d.textures
        );
    }
    static UBO_SHOWN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if draw_diag("ubo", &UBO_SHOWN) {
        ubo_diag(sh, n, d);
    }
    if crate::diag("tex") {
        let unresolved: Vec<_> = d
            .textures
            .iter()
            .filter(|t| t.handle != 0 && !sh.env.handles.contains_key(&t.handle))
            .map(|t| (t.stage, t.slot, format!("{:#x}", t.handle)))
            .collect();
        static SEEN: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());
        if !unresolved.is_empty() {
            let key = (
                format!("{:?}", d.program.fragment),
                format!("{unresolved:?}"),
            );
            if let Ok(mut seen) = SEEN.lock()
                && !seen.contains(&key)
                && seen.len() < 400
            {
                seen.push(key);
                eprintln!(
                    "DIAG tex unresolved draw {n} pass {:?} vs {} fs {:?} {unresolved:?}",
                    pc.size, d.program.vertex, d.program.fragment
                );
            }
        }
    }
    let p = prepare(
        sh,
        pc,
        d,
        &mut ScratchSink::new(scratch, sh.uploaded, Some(memo)).with_preloaded(sh.preloaded),
        false,
    );
    if crate::diag("pixel") && crate::diag::sampled() && FOCUS_PASS.with(std::cell::Cell::get) {
        static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        if SHOWN
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .is_multiple_of(97)
        {
            eprintln!(
                "DIAG pixel draw {:?} vs {} fs {:?} tex {:?} args {:?} vp {:?}",
                p.as_ref().err(),
                d.program.vertex,
                d.program.fragment,
                d.textures,
                d.args,
                d.dynamic
            );
            for t in d
                .textures
                .iter()
                .filter(|t| t.stage == crate::detail::Stage::Fragment && t.slot == 0)
            {
                eprintln!(
                    "DIAG pixel draw fs0 handle {:#x} resolved {:?}",
                    t.handle,
                    sh.env.handles.get(&t.handle).map(|r| r.texture)
                );
            }
        }
    }
    let copies = match pre.copies.get(&n).copied() {
        Some(Err(why)) => return stats.skip_draw(Some(&d.args), why),
        c => c.map(|c| c.unwrap_or_default()),
    };
    let p = match p {
        Ok(p) => p,
        Err(why) => return stats.skip_draw(Some(&d.args), why),
    };
    let x = Driven { object, copies };
    match emit(rp, tr, pc, &d.dynamic, &p, &x, stats) {
        Ok(()) => {
            stats.draws += 1;
            if let Some(c) = crate::stats::DrawClass::of(&d.args) {
                stats.kinds.encoded[c as usize] += 1;
            }
        }
        Err(_) => stats.skip_draw(Some(&d.args), Skip::Encode),
    }
}

/// Runs a replayed recording as one ICB. Returns false when the run gets no ICB and the caller encodes it draw by draw.
pub(super) fn run_icb(
    sh: &Shared<'_>,
    pc: &PassCtx,
    rp: &mut RenderPass<'_>,
    tr: &mut Tracker,
    run: &RecordingRun,
    stats: &mut EncodeStats,
) -> bool {
    let first = run.first_draw as usize;
    let Some(draws) = sh.detail.draws.get(first..first + run.draws as usize) else {
        return false;
    };
    let built = sh
        .icbs
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(sh, pc, run, draws);
    let (Some(d0), Some(b)) = (draws.first(), built) else {
        return false;
    };
    let Some(cull) = translate::cull(d0.raster.cull) else {
        return false;
    };
    let go = tr.dynamic(rp, pc, &d0.dynamic, cull).and_then(|()| {
        rp.encode(&RenderCommand::ExecuteIcb {
            icb: b.icb,
            first: 0,
            count: b.slots,
        })
    });
    if go.is_err() {
        stats.skipped[Skip::Encode as usize] += run.draws;
        return true;
    }
    tr.after_icb();
    stats.icb_executions += 1;
    stats.icb_draws += b.slots;
    stats.draws += b.slots;
    stats.pipeline_switches += 1;
    stats.icb_builds += u32::from(b.rebuilt);
    for why in b.skipped {
        stats.skip(why);
    }
    true
}
