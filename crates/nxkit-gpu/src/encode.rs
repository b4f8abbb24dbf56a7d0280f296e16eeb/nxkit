//! Render pass encoding.
//!
//! A render pass of the plan becomes one Metal render pass. The attachments, load actions, store actions and the
//! memoryless choice come from the analysis. The draws go in one by one, or a replayed recording goes in as one
//! indirect command buffer execution. A [`Tracker`] remembers the encoder state, so that a draw only sets what
//! changed.
//!
//! The viewport and the scissor rectangle of NVN are in the size of the game. When the attachments are scaled (R19)
//! the pass multiplies them by the scale of the frame. The NVN y origin of the viewport is passed on unchanged.
//! Whether it needs a flip is unverified. The check is a frame with a viewport that does not cover the target.

use std::sync::PoisonError;

use nxkit_metal::{
    ArgumentTable, ColorAttachment, CommandBuffer, Context, CullMode, DepthAttachment, DepthBias, DepthClip,
    DepthStencilDesc, FillMode, LoadAction as MLoad, MetalError, RenderCommand, RenderPass, RenderPassDesc, Scissor,
    Scratch, StencilAttachment, Storage, StoreAction as MStore, Viewport, Winding,
};
use nxkit_nvn::frame::Cmd;
use nxkit_nvn::passes::{LoadAction, Pass, Slot, StoreAction};

use crate::bindless::ScratchSink;
use crate::detail::{ClearDetail, DynamicDetail, RecordingRun};
use crate::draws::{Geom, PassCtx, Prep, Shared, prepare};
use crate::pipelines::Targets;
use crate::stats::{EncodeStats, Skip};
use crate::translate;

/// The argument tables of one encoding task. A vertex and a fragment stage need their own table.
pub(crate) struct Tables {
    pub(crate) vertex: ArgumentTable,
    pub(crate) fragment: ArgumentTable,
}

impl Tables {
    /// Makes the pair. A translated shader binds at most 31 buffers and no textures, since textures are bindless.
    pub(crate) fn new(ctx: &Context) -> Result<Self, MetalError> {
        Ok(Self { vertex: ArgumentTable::new(ctx, 31, 0)?, fragment: ArgumentTable::new(ctx, 31, 0)? })
    }
}

struct Setup {
    desc: RenderPassDesc,
    pc: PassCtx,
}

fn load(a: LoadAction, memoryless: bool) -> MLoad {
    match a {
        LoadAction::Clear => MLoad::Clear,
        LoadAction::Load if !memoryless => MLoad::Load,
        _ => MLoad::DontCare,
    }
}

fn store(a: StoreAction, memoryless: bool) -> MStore {
    if memoryless || a == StoreAction::DontCare { MStore::DontCare } else { MStore::Store }
}

/// The clear values that a pass starts with. The analysis cut the pass at the first full clear of a slot, so the
/// command is inside the pass.
fn clear_of(sh: &Shared<'_>, pass: &Pass, slot: Slot) -> ClearDetail {
    for i in pass.cmds.clone() {
        let hit = match (sh.frame.cmds.get(i as usize), slot) {
            (Some(Cmd::ClearColor { slot: s, full: true }), Slot::Color(n)) => *s == n,
            (Some(Cmd::ClearDepthStencil { full: true }), Slot::Depth) => true,
            _ => false,
        };
        if hit {
            let at = sh.ordinals.get(i as usize).map_or(0, |o| o.clear as usize);
            return sh.detail.clears.get(at).copied().unwrap_or_default();
        }
    }
    ClearDetail::default()
}

fn setup(sh: &Shared<'_>, pass: &Pass) -> Result<Setup, Skip> {
    let mut colors: Vec<(u8, ColorAttachment)> = Vec::new();
    let mut desc = RenderPassDesc::default();
    let mut size: Option<[u32; 2]> = None;
    let mut scale = 1.0;
    let mut has_stencil = false;
    for a in sh.plan.attachments_of(pass) {
        let res = sh.textures.get(&a.tex).ok_or(Skip::Texture)?;
        let d = res.desc;
        let mem = d.storage == Storage::Memoryless;
        if *size.get_or_insert([d.width, d.height]) != [d.width, d.height] {
            return Err(Skip::Target);
        }
        if res.scaled {
            scale = sh.scale;
        }
        let (l, s) = (load(a.load, mem), store(a.store, mem));
        let clear = clear_of(sh, pass, a.slot);
        match a.slot {
            Slot::Color(n) => colors.push((
                n,
                ColorAttachment { texture: res.tex.clone(), load: l, store: s, clear: clear.color.map(f64::from), memoryless: mem },
            )),
            Slot::Depth => {
                desc.depth = Some(DepthAttachment {
                    texture: res.tex.clone(),
                    load: l,
                    store: s,
                    clear: f64::from(clear.depth),
                    memoryless: mem,
                });
                if d.format.has_stencil() {
                    has_stencil = true;
                    desc.stencil = Some(StencilAttachment {
                        texture: res.tex.clone(),
                        load: l,
                        store: s,
                        clear: clear.stencil,
                        memoryless: mem,
                    });
                }
            }
        }
    }
    colors.sort_by_key(|(n, _)| *n);
    if colors.iter().enumerate().any(|(i, (n, _))| usize::from(*n) != i) {
        return Err(Skip::Target);
    }
    let size = size.ok_or(Skip::Target)?;
    let formats = colors.iter().map(|(_, c)| Some(c.texture.desc().format)).collect();
    desc.colors = colors.into_iter().map(|(_, c)| c).collect();
    let pc = PassCtx {
        targets: Targets { colors: formats, samples: 1 },
        size,
        scale,
        has_depth: desc.depth.is_some(),
        has_stencil,
    };
    Ok(Setup { desc, pc })
}

fn draw_range(sh: &Shared<'_>, pass: &Pass) -> std::ops::Range<u32> {
    let first = sh.ordinals.get(pass.cmds.start as usize).map_or(0, |o| o.draw);
    let n = pass.cmds.clone().filter(|i| matches!(sh.frame.cmds.get(*i as usize), Some(Cmd::Draw { .. }))).count();
    first..first + n as u32
}

/// What the encoder has set. It starts empty, so the first draw sets everything.
struct Tracker {
    size: [u32; 2],
    pipeline: Option<u64>,
    depth_stencil: Option<DepthStencilDesc>,
    cull: Option<CullMode>,
    winding: Option<Winding>,
    fill: Option<FillMode>,
    bias: Option<DepthBias>,
    clip: Option<DepthClip>,
    dynamic: Option<DynamicDetail>,
    streams: [Option<u64>; 32],
}

impl Tracker {
    fn new(size: [u32; 2]) -> Self {
        Self {
            size,
            pipeline: None,
            depth_stencil: None,
            cull: None,
            winding: None,
            fill: None,
            bias: None,
            clip: None,
            dynamic: None,
            streams: [None; 32],
        }
    }

    /// The state that an ICB execution leaves undefined.
    fn after_icb(&mut self) {
        let (size, dynamic, cull) = (self.size, self.dynamic, self.cull);
        *self = Self::new(size);
        self.dynamic = dynamic;
        self.cull = cull;
    }

    fn scissor(&self, s: Option<[u32; 4]>, scale: f32) -> Scissor {
        let [w, h] = self.size;
        let Some([x, y, sw, sh]) = s else { return Scissor { x: 0, y: 0, width: w, height: h } };
        let up = |v: u32| (v as f32 * scale).round() as u32;
        let (x, y) = (up(x).min(w.saturating_sub(1)), up(y).min(h.saturating_sub(1)));
        Scissor { x, y, width: up(sw).max(1).min((w - x).max(1)), height: up(sh).max(1).min((h - y).max(1)) }
    }

    /// Sets the viewport, the scissor rectangle, the stencil reference, the blend colour and the cull mode.
    fn dynamic(&mut self, rp: &mut RenderPass<'_>, pc: &PassCtx, d: &DynamicDetail, cull: CullMode) -> Result<(), MetalError> {
        let last = self.dynamic;
        if last.is_none_or(|l| l.viewport != d.viewport || l.depth_range != d.depth_range) {
            let [x, y, w, h] = d.viewport.map(|v| f64::from(v * pc.scale));
            let [near, far] = d.depth_range.map(f64::from);
            rp.encode(&RenderCommand::SetViewport(Viewport { x, y, width: w, height: h, z_near: near, z_far: far }))?;
        }
        if last.is_none_or(|l| l.scissor != d.scissor) {
            rp.encode(&RenderCommand::SetScissor(self.scissor(d.scissor, pc.scale)))?;
        }
        if last.is_none_or(|l| l.stencil_ref != d.stencil_ref) {
            rp.encode(&RenderCommand::SetStencilRef { front: d.stencil_ref[0], back: d.stencil_ref[1] })?;
        }
        if last.is_none_or(|l| l.blend_color != d.blend_color) {
            rp.encode(&RenderCommand::SetBlendColor(d.blend_color))?;
        }
        if self.cull != Some(cull) {
            rp.encode(&RenderCommand::SetCullMode(cull))?;
            self.cull = Some(cull);
        }
        self.dynamic = Some(*d);
        Ok(())
    }
}

fn emit(
    rp: &mut RenderPass<'_>,
    tr: &mut Tracker,
    pc: &PassCtx,
    d: &DynamicDetail,
    p: &Prep,
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
    rp.encode(&RenderCommand::SetBuffer { index: 0, gpu_addr: p.vertex_args })?;
    if let Some(a) = p.fragment_args {
        rp.encode(&RenderCommand::SetFragmentBuffer { index: 0, gpu_addr: a })?;
    }
    for (slot, addr) in &p.streams {
        let at = *slot as usize;
        if tr.streams.get(at).copied().flatten() != Some(*addr) {
            rp.encode(&RenderCommand::SetBuffer { index: *slot, gpu_addr: *addr })?;
            if let Some(c) = tr.streams.get_mut(at) {
                *c = Some(*addr);
            }
        }
    }
    rp.encode(&match p.geom {
        Geom::Arrays { primitive, first, count, instances, base_instance } => RenderCommand::Draw {
            primitive,
            first_vertex: first,
            vertex_count: count,
            instance_count: instances,
            base_instance,
        },
        Geom::Indexed { primitive, index_type, address, count, instances, base_vertex, base_instance } => {
            RenderCommand::DrawIndexed {
                primitive,
                index_type,
                index_addr: address,
                index_bytes: u64::from(count) * index_type.bytes(),
                index_count: count,
                instance_count: instances,
                base_vertex,
                base_instance,
            }
        }
    })
}

fn draw(
    sh: &Shared<'_>,
    pc: &PassCtx,
    rp: &mut RenderPass<'_>,
    tr: &mut Tracker,
    scratch: &Scratch,
    n: u32,
    stats: &mut EncodeStats,
) {
    let Some(d) = sh.detail.draws.get(n as usize) else { return stats.skip(Skip::Detail) };
    let p = match prepare(sh, pc, d, &mut ScratchSink(scratch), false) {
        Ok(p) => p,
        Err(why) => return stats.skip(why),
    };
    match emit(rp, tr, pc, &d.dynamic, &p, stats) {
        Ok(()) => stats.draws += 1,
        Err(_) => stats.skip(Skip::Encode),
    }
}

/// Runs a replayed recording as one ICB. Returns false when the run gets no ICB and the caller encodes it draw by draw.
fn run_icb(
    sh: &Shared<'_>,
    pc: &PassCtx,
    rp: &mut RenderPass<'_>,
    tr: &mut Tracker,
    run: &RecordingRun,
    stats: &mut EncodeStats,
) -> bool {
    let first = run.first_draw as usize;
    let Some(draws) = sh.detail.draws.get(first..first + run.draws as usize) else { return false };
    let built = sh.icbs.lock().unwrap_or_else(PoisonError::into_inner).get(sh, pc, run, draws);
    let (Some(d0), Some(b)) = (draws.first(), built) else { return false };
    let Some(cull) = translate::cull(d0.raster.cull) else { return false };
    let go = tr
        .dynamic(rp, pc, &d0.dynamic, cull)
        .and_then(|()| rp.encode(&RenderCommand::ExecuteIcb { icb: b.icb, first: 0, count: b.slots }));
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

/// Encodes one render pass into `cb`.
pub(crate) fn render_pass(
    sh: &Shared<'_>,
    cb: &mut CommandBuffer,
    tables: &Tables,
    scratch: &Scratch,
    index: usize,
    pass: &Pass,
    stats: &mut EncodeStats,
) {
    let range = draw_range(sh, pass);
    let lost = |stats: &mut EncodeStats, why: Skip| stats.skipped[why as usize] += range.len() as u32;
    let s = match setup(sh, pass) {
        Ok(s) => s,
        Err(why) => return lost(stats, why),
    };
    let Ok(mut rp) = cb.begin_render_pass_split(&s.desc, &tables.vertex, &tables.fragment) else {
        return lost(stats, Skip::Encode);
    };
    stats.render_passes += 1;
    if let Some(dep) = sh.barriers.before.get(index).copied().flatten() {
        if rp.encode(&RenderCommand::Depend(dep)).is_err() {
            stats.skipped[Skip::Encode as usize] += 1;
        }
    }
    let mut tr = Tracker::new(s.pc.size);
    let mut n = range.start;
    while n < range.end {
        if let Some(run) = sh.run_at.get(&n).filter(|r| n + r.draws <= range.end) {
            if run_icb(sh, &s.pc, &mut rp, &mut tr, run, stats) {
                n += run.draws;
                continue;
            }
        }
        draw(sh, &s.pc, &mut rp, &mut tr, scratch, n, stats);
        n += 1;
    }
    rp.end();
}
