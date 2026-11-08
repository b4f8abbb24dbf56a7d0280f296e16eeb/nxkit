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
    ArgumentTable, ColorAttachment, CommandBuffer, Context, CullMode, Dependency, DepthAttachment,
    DepthBias, DepthClip, DepthStencilDesc, FillMode, LoadAction as MLoad, MetalError,
    RenderCommand, RenderPass, RenderPassDesc, Scissor, Scope, Scratch, Stages, StencilAttachment,
    Storage, StoreAction as MStore, Viewport, Visibility, Winding,
};
use nxkit_nvn::frame::Cmd;
use nxkit_nvn::passes::{LoadAction, Pass, Slot, StoreAction};
use nxkit_nvn::resource::TexId;

use crate::bindless::{ArgsMemo, ScratchSink};
use crate::detail::{ClearDetail, DrawArgs, DynamicDetail, RecordingRun};
use crate::draws::{Geom, PassCtx, Prep, Shared, prepare};
use crate::indirect::Prepass;
use crate::pipelines::Targets;
use crate::stats::{EncodeStats, Skip};
use crate::translate;

/// The argument tables of one encoding task. A vertex and a fragment stage need their own table. The mesh stage of a
/// mesh draw shares the vertex table and the object stage (a task shader) has the third.
pub(crate) struct Tables {
    pub(crate) vertex: ArgumentTable,
    pub(crate) fragment: ArgumentTable,
    pub(crate) object: ArgumentTable,
}

impl Tables {
    /// Makes the tables. A translated shader binds at most 31 buffers and no textures, since textures are bindless.
    pub(crate) fn new(ctx: &Context) -> Result<Self, MetalError> {
        Ok(Self {
            vertex: ArgumentTable::new(ctx, 31, 0)?,
            fragment: ArgumentTable::new(ctx, 31, 0)?,
            object: ArgumentTable::new(ctx, 31, 0)?,
        })
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
    if memoryless || a == StoreAction::DontCare {
        MStore::DontCare
    } else {
        MStore::Store
    }
}

/// The clear values that a pass starts with. The analysis cut the pass at the first full clear of a slot, so the
/// command is inside the pass.
fn clear_of(sh: &Shared<'_>, pass: &Pass, slot: Slot) -> ClearDetail {
    for i in pass.cmds.clone() {
        let hit = match (sh.frame.cmds.get(i as usize), slot) {
            (
                Some(Cmd::ClearColor {
                    slot: s,
                    full: true,
                }),
                Slot::Color(n),
            ) => *s == n,
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

/// True when the first draw of the pass visibly paints every texel of colour slot `slot` of an attachment of
/// `size` (after the scale `scale`): a full-screen triangle (or quad) with no vertex input, a viewport and a scissor
/// that hold the target, all four channels written, no blending, no depth or stencil test, no alpha to coverage.
/// Then the old contents are never read and a `Load` is wasted. The shader could still discard a fragment, and the
/// geometry of a vertex shader without input could still be small, so only the `coverload` knob uses this.
fn first_draw_covers(sh: &Shared<'_>, pass: &Pass, slot: u8, size: [u32; 2], scale: f32) -> bool {
    let Some(d) = draw_range(sh, pass)
        .next()
        .and_then(|n| sh.detail.draws.get(n as usize))
    else {
        return false;
    };
    let vertices_ok = matches!(
        (d.topology, d.args),
        (
            4,
            DrawArgs::Arrays {
                count: 3 | 6,
                instances: 1,
                ..
            }
        ) | (
            5,
            DrawArgs::Arrays {
                count: 4,
                instances: 1,
                ..
            }
        )
    );
    let [x, y, w, h] = d.dynamic.viewport.map(|v| v * scale);
    let holds = |x0: f32, y0: f32, w0: f32, h0: f32| {
        x0 <= 0.5 && y0 <= 0.5 && x0 + w0 >= size[0] as f32 - 0.5 && y0 + h0 >= size[1] as f32 - 0.5
    };
    let scissor_ok = d.dynamic.scissor.is_none_or(|[sx, sy, sw, shh]| {
        holds(
            sx as f32 * scale,
            sy as f32 * scale,
            sw as f32 * scale,
            shh as f32 * scale,
        )
    });
    let i = usize::from(slot).min(7);
    vertices_ok
        && d.attributes.is_empty()
        && d.streams.is_empty()
        && holds(x, y, w, h)
        && scissor_ok
        && d.write_masks[i] & 0xf == 0xf
        && !d.blend[i].enable
        && !d.depth_stencil.depth_test
        && !d.depth_stencil.stencil_test
        && !d.raster.alpha_to_coverage
        && d.raster.polygon_mode == 2
}

fn setup(sh: &Shared<'_>, pass: &Pass) -> Result<Setup, Skip> {
    let attachments = sh.plan.attachments_of(pass);
    let color_aliases: Vec<_> = attachments
        .iter()
        .filter_map(|a| {
            let Slot::Color(slot) = a.slot else {
                return None;
            };
            let res = sh.textures.get(&a.tex)?;
            Some((
                slot,
                (res.barrier_id, a.view.level(), a.view.layer()),
                a.load == LoadAction::Clear,
            ))
        })
        .collect();
    let has_aliases = color_aliases
        .iter()
        .enumerate()
        .any(|(i, a)| color_aliases[..i].iter().any(|b| a.1 == b.1));
    let selected = if has_aliases {
        let mut written = 0u8;
        let mut outputs = crate::draws::FastMap::default();
        for n in draw_range(sh, pass) {
            let Some(d) = sh.detail.draws.get(n as usize) else {
                continue;
            };
            let Some(key) = &d.program.fragment else {
                continue;
            };
            let locations = if let Some(mask) = outputs.get(key) {
                *mask
            } else {
                let fs = sh.programs.stage(key, false)?;
                let mask = fs.colors.as_ref().map_or(u8::MAX, |colors| {
                    colors
                        .iter()
                        .filter(|c| **c < 8)
                        .fold(0, |m, c| m | (1 << c))
                });
                outputs.insert(key, mask);
                mask
            };
            for slot in 0..8 {
                if d.write_masks[slot] != 0 && locations & (1 << slot) != 0 {
                    written |= 1 << slot;
                }
            }
        }
        match distinct_color_slots(&color_aliases, written) {
            Ok(selected) => selected,
            Err(why) => {
                if crate::diag("target") && crate::diag::sampled() {
                    eprintln!(
                        "DIAG alias conflict cmds {:?} attachments {:?}",
                        pass.cmds, color_aliases
                    );
                    for n in draw_range(sh, pass) {
                        if let Some(d) = sh.detail.draws.get(n as usize) {
                            eprintln!(
                                "DIAG alias draw {n} fs {:?} masks {:?}",
                                d.program.fragment, d.write_masks
                            );
                        }
                    }
                }
                return Err(why);
            }
        }
    } else {
        u8::MAX
    };
    let mut colors: Vec<(u8, ColorAttachment)> = Vec::new();
    let mut desc = RenderPassDesc::default();
    let mut size: Option<[u32; 2]> = None;
    let mut scale = 1.0;
    let mut has_stencil = false;
    for a in sh.plan.attachments_of(pass) {
        if matches!(a.slot, Slot::Color(n) if selected & (1 << n) == 0) {
            continue;
        }
        let res = sh.textures.get(&a.tex).ok_or(Skip::Texture)?;
        let texture = sh.texture_store.target_view(res, a.view)?;
        let d = texture.desc();
        if crate::diag("draw")
            && crate::diag::sampled()
            && crate::diag::tex_match(d.format, d.width, d.height, d.array_layers)
        {
            eprintln!(
                "DIAG attachment {:?} {:?} backing {:?} {:?} {}x{} cmds {:?}",
                a.slot, a.tex, res.barrier_id, d.format, d.width, d.height, pass.cmds
            );
        }
        // The game encodes block compressed textures on the GPU by rendering into them. Metal cannot render into a
        // compressed format, so such a pass is left out.
        if matches!(a.slot, Slot::Color(_)) && (d.format.is_compressed() || d.format.is_depth()) {
            target_diag(format_args!(
                "not renderable {:?} {}x{} cmds {:?}",
                d.format, d.width, d.height, pass.cmds
            ));
            nxkit_nvn::diag::example(
                nxkit_nvn::diag::Reason::SkipFormat,
                format_args!(
                    "colour target {:?} {:?} {}x{}x{}",
                    a.slot, d.format, d.width, d.height, d.depth
                ),
            );
            return Err(Skip::Format);
        }
        let mem = d.storage == Storage::Memoryless;
        // Attachments of different sizes render into the smallest one, as in NVN.
        let s = size.get_or_insert([d.width, d.height]);
        if *s != [d.width, d.height] {
            target_diag(format_args!(
                "size mismatch {:?} vs {}x{} slot {:?}",
                s, d.width, d.height, a.slot
            ));
            *s = [s[0].min(d.width), s[1].min(d.height)];
        }
        if res.scaled {
            scale = sh.scale;
        }
        let mut l = load(a.load, mem);
        // A colour load that only the default rule asks for goes when the draw covers the target.
        if let Slot::Color(n) = a.slot
            && a.load_is_default()
            && !mem
            && l == MLoad::Load
        {
            let covers = first_draw_covers(
                sh,
                pass,
                n,
                [d.width, d.height],
                if res.scaled { sh.scale } else { 1.0 },
            );
            if crate::diag("passmerge") && crate::diag::sampled() {
                eprintln!(
                    "DIAG passmerge cover cmds {:?} slot {n} {}x{} first draw covers {covers}",
                    pass.cmds, d.width, d.height
                );
            }
            if covers && crate::knob("coverload") {
                l = MLoad::DontCare;
            }
        }
        let s = store(a.store, mem);
        let clear = clear_of(sh, pass, a.slot);
        match a.slot {
            Slot::Color(n) => colors.push((
                n,
                ColorAttachment {
                    depth_plane: if d.kind == nxkit_metal::TextureKind::D3 {
                        a.view.layer()
                    } else {
                        0
                    },
                    texture: texture.clone(),
                    load: l,
                    store: s,
                    clear: clear.color.map(f64::from),
                    memoryless: mem,
                },
            )),
            Slot::Depth => {
                if crate::diag("depth") {
                    static SHOWN: std::sync::atomic::AtomicU32 =
                        std::sync::atomic::AtomicU32::new(0);
                    if SHOWN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 500 < 20 {
                        eprintln!(
                            "DIAG depth pass {:?} {}x{} {:?} load {:?} store {:?} clear {} cmds {:?}",
                            a.tex,
                            d.width,
                            d.height,
                            d.format,
                            a.load,
                            a.store,
                            clear.depth,
                            pass.cmds
                        );
                    }
                }
                desc.depth = Some(DepthAttachment {
                    texture: texture.clone(),
                    load: l,
                    store: s,
                    clear: f64::from(clear.depth),
                    memoryless: mem,
                });
                if d.format.has_stencil() {
                    has_stencil = true;
                    desc.stencil = Some(StencilAttachment {
                        texture: texture.clone(),
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
    if colors.windows(2).any(|w| w[0].0 == w[1].0) {
        target_diag(format_args!(
            "twice {:?}",
            colors.iter().map(|(n, _)| *n).collect::<Vec<_>>()
        ));
        return Err(Skip::Target);
    }
    if size.is_none() {
        target_diag(format_args!("none, cmds {:?}", pass.cmds));
    }
    let size = size.ok_or(Skip::Target)?;
    // NVN allows empty colour slots between attachments. They stay empty in the pass and in the pipeline.
    let slots = colors.last().map_or(0, |(n, _)| usize::from(*n) + 1);
    let mut formats = vec![None; slots];
    desc.colors = vec![None; slots];
    for (n, c) in colors {
        formats[usize::from(n)] = Some(c.texture.desc().format);
        desc.colors[usize::from(n)] = Some(c);
    }
    let pc = PassCtx {
        targets: Targets {
            colors: formats,
            samples: 1,
        },
        size,
        scale,
        has_depth: desc.depth.is_some(),
        has_stencil,
    };
    Ok(Setup { desc, pc })
}

/// NVN may bind a texture to several slots and select the writer with channel masks. Metal gives each attachment
/// its own tile storage, so storing an inactive alias can overwrite the active output. Preserve the slot of the
/// actual writer (or clear), leaving the other aliases empty rather than compacting the output locations.
fn distinct_color_slots<K: Copy + Eq + std::fmt::Debug>(
    colors: &[(u8, K, bool)],
    written: u8,
) -> Result<u8, Skip> {
    let mut selected = 0u8;
    for (i, &(slot, backing, _)) in colors.iter().enumerate() {
        if colors[..i].iter().any(|c| c.1 == backing) {
            continue;
        }
        let aliases: Vec<_> = colors.iter().filter(|c| c.1 == backing).collect();
        let writers: Vec<_> = aliases
            .iter()
            .filter(|c| written & (1 << c.0) != 0)
            .collect();
        if writers.len() > 1 {
            if crate::diag("target") && crate::diag::sampled() {
                eprintln!("DIAG target competing aliases {backing:?} written {written:#x}");
            }
            return Err(Skip::Target);
        }
        let winner = writers
            .first()
            .map(|c| c.0)
            .or_else(|| aliases.iter().find(|c| c.2).map(|c| c.0))
            .unwrap_or(slot);
        selected |= 1 << winner;
    }
    Ok(selected)
}

/// Prints a line of the `target` category, at most 300.
fn target_diag(what: std::fmt::Arguments<'_>) {
    static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    if crate::diag("target") && crate::diag::first(&SHOWN, 300) {
        eprintln!("DIAG target {what}");
    }
}

thread_local! {
    /// True while the encoded render pass has an attachment that passes `NXKIT_DIAG_TEX`, or always without it. The
    /// per-draw categories print the draws of such passes only.
    static FOCUS_PASS: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// The most lines that one per-draw category prints on one sampled frame.
const DRAW_LINES: u32 = 2000;

/// Whether a per-draw category prints this draw: on sampled frames, in focused passes, within [`DRAW_LINES`].
fn draw_diag(cat: &str, budget: &std::sync::atomic::AtomicU64) -> bool {
    crate::diag(cat)
        && crate::diag::sampled()
        && FOCUS_PASS.with(std::cell::Cell::get)
        && crate::diag::sample_budget(budget, DRAW_LINES)
}

fn draw_range(sh: &Shared<'_>, pass: &Pass) -> std::ops::Range<u32> {
    let first = sh
        .ordinals
        .get(pass.cmds.start as usize)
        .map_or(0, |o| o.draw);
    let n = pass
        .cmds
        .clone()
        .filter(|i| matches!(sh.frame.cmds.get(*i as usize), Some(Cmd::Draw { .. })))
        .count();
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
    vertex_args: Option<u64>,
    fragment_args: Option<u64>,
    object_args: Option<u64>,
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
            vertex_args: None,
            fragment_args: None,
            object_args: None,
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
        let Some([x, y, sw, sh]) = s else {
            return Scissor {
                x: 0,
                y: 0,
                width: w,
                height: h,
            };
        };
        let up = |v: u32| (v as f32 * scale).round() as u32;
        let (x, y) = (
            up(x).min(w.saturating_sub(1)),
            up(y).min(h.saturating_sub(1)),
        );
        Scissor {
            x,
            y,
            width: up(sw).max(1).min((w - x).max(1)),
            height: up(sh).max(1).min((h - y).max(1)),
        }
    }

    /// Sets the viewport, the scissor rectangle, the stencil reference, the blend colour and the cull mode.
    fn dynamic(
        &mut self,
        rp: &mut RenderPass<'_>,
        pc: &PassCtx,
        d: &DynamicDetail,
        cull: CullMode,
    ) -> Result<(), MetalError> {
        let last = self.dynamic;
        if last.is_none_or(|l| l.viewport != d.viewport || l.depth_range != d.depth_range) {
            let [x, y, w, h] = d.viewport.map(|v| f64::from(v * pc.scale));
            let [near, far] = d.depth_range.map(f64::from);
            rp.encode(&RenderCommand::SetViewport(Viewport {
                x,
                y,
                width: w,
                height: h,
                z_near: near,
                z_far: far,
            }))?;
        }
        if last.is_none_or(|l| l.scissor != d.scissor) {
            rp.encode(&RenderCommand::SetScissor(
                self.scissor(d.scissor, pc.scale),
            ))?;
        }
        if last.is_none_or(|l| l.stencil_ref != d.stencil_ref) {
            rp.encode(&RenderCommand::SetStencilRef {
                front: d.stencil_ref[0],
                back: d.stencil_ref[1],
            })?;
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

/// What a GPU driven draw needs besides its [`Prep`]: the table of the object stage and the records that the pre-pass
/// of the render pass copied for it.
struct Driven<'a> {
    object: &'a ArgumentTable,
    copies: Option<u64>,
}

fn emit(
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
fn draw(
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
            if let Ok(mut seen) = SEEN.lock() {
                if !seen.contains(&key) && seen.len() < 400 {
                    seen.push(key);
                    eprintln!(
                        "DIAG tex unresolved draw {n} pass {:?} vs {} fs {:?} {unresolved:?}",
                        pc.size, d.program.vertex, d.program.fragment
                    );
                }
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
        if SHOWN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 97 == 0 {
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

/// The `ubo` category: the uniform buffers of draw `n` as the guest memory holds them now and as the snapshot of the
/// walk holds them, and the first floats of its first two vertex streams.
fn ubo_diag(sh: &Shared<'_>, n: u32, d: &crate::detail::DrawDetail) {
    let floats = |addr: u64, count: usize| -> Vec<f32> {
        let mut bytes = vec![0u8; count * 4];
        match sh.guest_offset(addr, bytes.len() as u64) {
            Some(off) if sh.guest.read(off, &mut bytes).is_ok() => bytes
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect(),
            _ => Vec::new(),
        }
    };
    for u in &d.ubos {
        let count = (u.size as usize / 4).min(16);
        let snap: Option<Vec<f32>> = u.snapshot.as_ref().map(|b| {
            b.chunks_exact(4)
                .take(count)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect()
        });
        eprintln!(
            "DIAG ubo draw {n} {:?} slot {} addr {:#x} size {} live {:?} snapshot {snap:?}",
            u.stage,
            u.slot,
            u.address,
            u.size,
            floats(u.address, count)
        );
    }
    for st in d.streams.iter().take(2) {
        eprintln!(
            "DIAG ubo draw {n} stream {} addr {:#x} stride {} floats {:?}",
            st.index,
            st.address,
            st.stride,
            floats(st.address, 8)
        );
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
    if crate::diag::tex_filtered() {
        let focus = sh.plan.attachments_of(pass).iter().any(|a| {
            sh.textures.get(&a.tex).is_some_and(|r| {
                crate::diag::tex_match(
                    r.desc.format,
                    r.desc.width,
                    r.desc.height,
                    r.desc.array_layers,
                )
            })
        });
        FOCUS_PASS.with(|c| c.set(focus));
    }
    let lost = |stats: &mut EncodeStats, why: Skip| {
        for n in range.clone() {
            stats.skip_draw(sh.detail.draws.get(n as usize).map(|d| &d.args), why);
        }
    };
    if crate::diag("barr") && crate::diag::sampled() {
        let known: std::collections::HashSet<TexId> = sh
            .plan
            .reads_of(pass)
            .iter()
            .copied()
            .chain(sh.plan.attachments_of(pass).iter().map(|a| a.tex))
            .collect();
        let mut missing: std::collections::BTreeSet<TexId> = std::collections::BTreeSet::new();
        let mut bound = 0usize;
        for n in range.clone() {
            if let Some(d) = sh.detail.draws.get(n as usize) {
                for t in d.textures.iter().filter(|t| t.handle != 0) {
                    bound += 1;
                    if let Some(id) = crate::barr::texture_of(t.handle) {
                        if !known.contains(&id) {
                            missing.insert(id);
                        }
                    }
                }
            }
        }
        eprintln!(
            "DIAG barr pass {index} cmds {:?} draws {} bound {bound} planner_reads {} missing {missing:?}",
            pass.cmds,
            range.len(),
            sh.plan.reads_of(pass).len()
        );
    }
    let s = setup(sh, pass);
    if sh
        .plan
        .attachments_of(pass)
        .iter()
        .any(|a| crate::watched(a.tex))
    {
        static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        if SHOWN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 40 {
            let atts: Vec<_> = sh
                .plan
                .attachments_of(pass)
                .iter()
                .map(|a| {
                    (
                        a.tex,
                        a.slot,
                        sh.textures
                            .get(&a.tex)
                            .map(|r| (r.desc.width, r.desc.height, r.desc.format)),
                    )
                })
                .collect();
            eprintln!(
                "DIAG watch pass {index} cmds {:?} draws {} setup {:?} atts {atts:?}",
                pass.cmds,
                range.len(),
                s.as_ref().err()
            );
        }
    }
    let s = match s {
        Ok(s) => s,
        Err(why) => return lost(stats, why),
    };
    // The GPU driven draws of the pass get their copied arguments before the pass starts.
    let pre = crate::indirect::prepass(sh, cb, &tables.vertex, scratch, range.clone(), stats);
    let Ok(mut rp) = cb.begin_render_pass_split(&s.desc, &tables.vertex, &tables.fragment) else {
        return lost(stats, Skip::Encode);
    };
    stats.render_passes += 1;
    if crate::diag::gpu_labels() {
        let first = s
            .desc
            .colors
            .iter()
            .flatten()
            .map(|c| c.texture.desc())
            .next()
            .or_else(|| s.desc.depth.as_ref().map(|d| d.texture.desc()));
        let what = first.map_or_else(String::new, |d| {
            format!("{}x{} {:?}", d.width, d.height, d.format)
        });
        rp.set_label(&format!("r{index} {what} d{}", range.len()));
    }
    if let Some(dep) = sh.barriers.before.get(index).copied().flatten() {
        if rp.encode(&RenderCommand::Depend(dep)).is_err() {
            stats.skipped[Skip::Encode as usize] += 1;
        }
    }
    // The barrier planner follows textures only, and it holds the vertex and fragment stages. An indirect draw reads
    // arguments that a dispatch or a copy wrote (culling), the pre-pass wrote the copies, and a task or mesh shader
    // reads storage buffers and textures in stages that the planner does not hold. Such a pass waits for them.
    if pre.gpu_driven {
        let dep = Dependency {
            scope: Scope::FromEarlierPasses,
            after: Stages::DISPATCH | Stages::BLIT | Stages::FRAGMENT,
            before: Stages::VERTEX | Stages::OBJECT | Stages::MESH,
            visibility: Visibility::Device,
        };
        match rp.encode(&RenderCommand::Depend(dep)) {
            Ok(()) => stats.barriers_emitted += 1,
            Err(_) => stats.skipped[Skip::Encode as usize] += 1,
        }
    }
    let mut tr = Tracker::new(s.pc.size);
    let mut memo = ArgsMemo::default();
    let mut n = range.start;
    while n < range.end {
        if let Some(run) = sh.run_at.get(&n).filter(|r| n + r.draws <= range.end) {
            if run_icb(sh, &s.pc, &mut rp, &mut tr, run, stats) {
                n += run.draws;
                continue;
            }
        }
        draw(
            sh,
            &s.pc,
            &mut rp,
            &mut tr,
            scratch,
            &mut memo,
            &pre,
            &tables.object,
            n,
            stats,
        );
        n += 1;
    }
    rp.end();
    if crate::lum::active() && FOCUS_PASS.with(std::cell::Cell::get) {
        for a in sh.plan.attachments_of(pass) {
            if matches!(a.slot, Slot::Color(_)) && a.store == StoreAction::Store {
                if let Slot::Color(slot) = a.slot {
                    let Some(Some(color)) = s.desc.colors.get(usize::from(slot)) else {
                        continue;
                    };
                    crate::lum::capture(
                        sh.ctx,
                        cb,
                        &tables.vertex,
                        pass.cmds.start,
                        a.tex,
                        &color.texture,
                        color.depth_plane,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxkit_metal::{
        ArchiveConfig, Format, FrameRing, PipelineCache, Primitive, RenderPipelineDesc,
        ShaderLibrary, Texture, TextureDesc,
    };

    #[test]
    fn inactive_aliases_leave_the_original_output_locations_empty() {
        let colors = [
            (0, TexId(1), false),
            (1, TexId(2), false),
            (2, TexId(2), false),
            (3, TexId(3), false),
            (4, TexId(3), false),
        ];
        assert_eq!(distinct_color_slots(&colors, 0b01011).unwrap(), 0b01011);
        assert_eq!(distinct_color_slots(&colors, 0b10101).unwrap(), 0b10101);
        assert_eq!(
            distinct_color_slots(&[(1, TexId(2), false), (2, TexId(2), true)], 0).unwrap(),
            4
        );
        assert!(distinct_color_slots(&colors, 0b00110).is_err());
    }

    #[test]
    fn an_inactive_alias_cannot_overwrite_a_rendered_color() {
        let Ok(ctx) = Context::system_default() else {
            return;
        };
        let selected =
            distinct_color_slots(&[(0, TexId(1), false), (1, TexId(1), false)], 1).unwrap();
        let lib = ShaderLibrary::from_source(
            &ctx,
            "alias-output",
            r#"
#include <metal_stdlib>
using namespace metal;
struct V { float4 pos [[position]]; };
vertex V vs(uint id [[vertex_id]]) {
    float2 p = float2((id << 1) & 2, id & 2);
    return {float4(p * 2 - 1, 0, 1)};
}
struct Outputs { float4 a [[color(0)]]; float4 b [[color(1)]]; };
fragment Outputs fs() { return {float4(1,0,0,1), float4(0)}; }
"#,
        )
        .unwrap();
        let mut texture_desc = TextureDesc::render_target_2d(16, 16, Format::Rgba8Unorm);
        texture_desc.storage = Storage::Shared;
        let texture = Texture::new(&ctx, texture_desc).unwrap();
        let mut pipeline_desc =
            RenderPipelineDesc::new((&lib, "vs"), Some((&lib, "fs")), &[Format::Rgba8Unorm; 2]);
        pipeline_desc.colors[1].as_mut().unwrap().write_mask = 0;
        let mut colors = vec![
            Some(ColorAttachment {
                depth_plane: 0,
                texture: texture.clone(),
                load: MLoad::Clear,
                store: MStore::Store,
                clear: [0.0; 4],
                memoryless: false
            });
            2
        ];
        for slot in 0..2 {
            if selected & (1 << slot) == 0 {
                colors[slot] = None;
                pipeline_desc.colors[slot] = None;
            }
        }
        let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
        let pipeline = cache.render(1, &pipeline_desc).unwrap();
        let table = ArgumentTable::new(&ctx, 1, 1).unwrap();
        let mut ring = FrameRing::new(&ctx).unwrap();
        let mut frame = ring.begin().unwrap();
        let mut pass = frame
            .commands()
            .begin_render_pass(
                &RenderPassDesc {
                    colors,
                    depth: None,
                    stencil: None,
                },
                &table,
            )
            .unwrap();
        pass.encode(&RenderCommand::SetPipeline(pipeline)).unwrap();
        pass.encode(&RenderCommand::Draw {
            primitive: Primitive::Triangle,
            first_vertex: 0,
            vertex_count: 3,
            instance_count: 1,
            base_instance: 0,
        })
        .unwrap();
        pass.end();
        frame.submit().unwrap().wait().unwrap();
        let mut pixels = vec![0; 16 * 16 * 4];
        texture.read(16 * 4, &mut pixels).unwrap();
        assert!(pixels.chunks_exact(4).all(|p| p == [255, 0, 0, 255]));
    }
}
