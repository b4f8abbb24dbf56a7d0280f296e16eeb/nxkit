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
use nxkit_nvn::resource::TexId;

use crate::bindless::{ArgsMemo, ScratchSink};
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
    let attachments = sh.plan.attachments_of(pass);
    let color_aliases: Vec<_> = attachments.iter().filter_map(|a| {
        let Slot::Color(slot) = a.slot else { return None };
        let res = sh.textures.get(&a.tex)?;
        Some((slot, (res.barrier_id, a.view.level(), a.view.layer()), a.load == LoadAction::Clear))
    }).collect();
    let has_aliases = color_aliases.iter().enumerate().any(|(i, a)|
        color_aliases[..i].iter().any(|b| a.1 == b.1));
    let selected = if has_aliases {
        let mut written = 0u8;
        let mut outputs = crate::draws::FastMap::default();
        for n in draw_range(sh, pass) {
            let Some(d) = sh.detail.draws.get(n as usize) else { continue };
            let Some(key) = &d.program.fragment else { continue };
            let locations = if let Some(mask) = outputs.get(key) { *mask } else {
                let fs = sh.programs.stage(key, false)?;
                let mask = fs.colors.as_ref().map_or(u8::MAX, |colors| colors.iter().filter(|c| **c < 8).fold(0, |m, c| m | (1 << c)));
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
                if crate::diag("target") && crate::lum::active() {
                    eprintln!("DIAG alias conflict cmds {:?} attachments {:?}", pass.cmds, color_aliases);
                    for n in draw_range(sh, pass) {
                        if let Some(d) = sh.detail.draws.get(n as usize) {
                            eprintln!("DIAG alias draw {n} fs {:?} masks {:?}", d.program.fragment, d.write_masks);
                        }
                    }
                }
                return Err(why);
            }
        }
    } else { u8::MAX };
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
        if crate::diag("cube") && d.width <= 128 && d.format == nxkit_metal::Format::Rg11B10Float {
            static SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
            let line = format!(
                "DIAG cube pass cmds {:?} {:?} {:?} {}x{} {:?} level {} layer {} load {:?} store {:?}",
                pass.cmds, a.slot, a.tex, d.width, d.height, d.kind, a.view.level(), a.view.layer(), a.load, a.store
            );
            if let Ok(mut seen) = SEEN.lock() {
                let key = format!("{:?}{:?}{}{}", a.tex, a.slot, a.view.level(), a.view.layer());
                if !seen.contains(&key) && seen.len() < 3000 {
                    seen.push(key);
                    eprintln!("{line}");
                }
            }
        }
        if crate::diag("draw") && crate::lum::active() && d.width == 1600 && d.height == 900 {
            eprintln!("DIAG attachment {:?} {:?} backing {:?} {:?} cmds {:?}", a.slot, a.tex, res.barrier_id, d.format, pass.cmds);
        }
        // The game encodes block compressed textures on the GPU by rendering into them. Metal cannot render into a
        // compressed format, so such a pass is left out.
        if matches!(a.slot, Slot::Color(_)) && (d.format.is_compressed() || d.format.is_depth()) {
            if crate::diag("target") {
                eprintln!("DIAG target not renderable {:?}", d.format);
            }
            return Err(Skip::Format);
        }
        let mem = d.storage == Storage::Memoryless;
        // Attachments of different sizes render into the smallest one, as in NVN.
        let s = size.get_or_insert([d.width, d.height]);
        if *s != [d.width, d.height] {
            if crate::diag("target") {
                eprintln!(
                    "DIAG target size mismatch {:?} vs {}x{} slot {:?}",
                    s, d.width, d.height, a.slot
                );
            }
            *s = [s[0].min(d.width), s[1].min(d.height)];
        }
        if res.scaled {
            scale = sh.scale;
        }
        let (l, s) = (load(a.load, mem), store(a.store, mem));
        let clear = clear_of(sh, pass, a.slot);
        match a.slot {
            Slot::Color(n) => colors.push((
                n,
                ColorAttachment { depth_plane: if d.kind == nxkit_metal::TextureKind::D3 { a.view.layer() } else { 0 },
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
        if crate::diag("target") {
            eprintln!(
                "DIAG target twice {:?}",
                colors.iter().map(|(n, _)| *n).collect::<Vec<_>>()
            );
        }
        return Err(Skip::Target);
    }
    if size.is_none() && crate::diag("target") {
        eprintln!("DIAG target none");
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
fn distinct_color_slots<K: Copy + Eq + std::fmt::Debug>(colors: &[(u8, K, bool)], written: u8) -> Result<u8, Skip> {
    let mut selected = 0u8;
    for (i, &(slot, backing, _)) in colors.iter().enumerate() {
        if colors[..i].iter().any(|c| c.1 == backing) { continue }
        let aliases: Vec<_> = colors.iter().filter(|c| c.1 == backing).collect();
        let writers: Vec<_> = aliases.iter().filter(|c| written & (1 << c.0) != 0).collect();
        if writers.len() > 1 {
            if crate::diag("target") && crate::lum::active() { eprintln!("DIAG target competing aliases {backing:?} written {written:#x}"); }
            return Err(Skip::Target);
        }
        let winner = writers.first().map(|c| c.0)
            .or_else(|| aliases.iter().find(|c| c.2).map(|c| c.0))
            .unwrap_or(slot);
        selected |= 1 << winner;
    }
    Ok(selected)
}

thread_local! {
    /// True while the encoded render pass writes a terrain array (the `hm` diagnostic).
    static HM_PASS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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

#[allow(clippy::too_many_arguments)]
fn draw(
    sh: &Shared<'_>,
    pc: &PassCtx,
    rp: &mut RenderPass<'_>,
    tr: &mut Tracker,
    scratch: &Scratch,
    memo: &mut ArgsMemo,
    n: u32,
    stats: &mut EncodeStats,
) {
    let Some(d) = sh.detail.draws.get(n as usize) else {
        return stats.skip(Skip::Detail);
    };
    if crate::diag("draw") && crate::lum::active() && pc.size == [1600, 900] {
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
    if crate::diag("cube")
        && crate::lum::active()
        && pc.size[0] <= 128
        && pc.targets.colors.first().copied().flatten() == Some(nxkit_metal::Format::Rg11B10Float)
    {
        static SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let key = format!("{n}{:?}{:?}{:?}", d.program.fragment, d.args, pc.size);
        if let Ok(mut seen) = SEEN.lock() {
            if !seen.contains(&key) && seen.len() < 3000 {
                seen.push(key);
                let unresolved: Vec<_> = d
                    .textures
                    .iter()
                    .filter(|t| t.handle != 0 && !sh.env.handles.contains_key(&t.handle))
                    .map(|t| t.handle)
                    .collect();
                eprintln!(
                    "DIAG cube draw {n} size {:?} vs {} fs {:?} args {:?} depth {}/{}/{} cull {} masks0 {} blend0 {:?} vp {:?} sc {:?} tex {} unresolved {unresolved:?}",
                    pc.size, d.program.vertex, d.program.fragment, d.args,
                    d.depth_stencil.depth_test, d.depth_stencil.depth_write, d.depth_stencil.depth_func,
                    d.raster.cull, d.write_masks[0], d.blend.first().map(|b| (b.enable, b.src_rgb, b.dst_rgb)),
                    d.dynamic.viewport, d.dynamic.scissor, d.textures.len()
                );
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
                for u in d.ubos.iter().filter(|u| u.slot <= 1) {
                    let live = floats(u.address, (u.size as usize / 4).min(80));
                    let snap: Vec<f32> = u
                        .snapshot
                        .as_ref()
                        .map(|b| b.chunks_exact(4).take(80).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
                        .unwrap_or_default();
                    eprintln!(
                        "DIAG cube ubo {n} {:?} slot {} addr {:#x} size {} live {live:?} snap {snap:?}",
                        u.stage, u.slot, u.address, u.size
                    );
                }
                eprintln!(
                    "DIAG cube tex {n} {:?}",
                    d.textures.iter().filter(|t| t.handle != 0).map(|t| (t.stage, t.slot, t.handle)).collect::<Vec<_>>()
                );
                for st in d.streams.iter().take(2) {
                    eprintln!(
                        "DIAG cube stream {n} index {} addr {:#x} size {} stride {} floats {:?} attrs {:?}",
                        st.index, st.address, st.size, st.stride, floats(st.address, 12), d.attributes
                    );
                }
            }
        }
    }
    if crate::diag("hmsrc") && HM_PASS.with(std::cell::Cell::get) {
        static SEEN: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());
        for t in d.textures.iter().filter(|t| t.handle != 0 && t.stage == crate::detail::Stage::Fragment && t.slot == 0) {
            let Ok((slot, _)) = sh.table.resolve(t.handle) else { continue };
            let Ok(rec) = sh.table.record(slot.texture) else { continue };
            let Ok(mut seen) = SEEN.lock() else { break };
            if seen.contains(&slot.texture.0) || seen.len() > 400 {
                continue;
            }
            seen.push(slot.texture.0);
            let map = sh.guest.address_map();
            let mut nz = 0;
            let mut buf = [0u8; 256];
            let pages = rec.layout.size.div_ceil(4096);
            let mut per_layer = vec![0u32; rec.layout.layers.max(1) as usize];
            if let Some(base) = rec.address.get().checked_sub(map.gpu_base()) {
                for p in 0..pages {
                    if sh.guest.read(base + p * 4096, &mut buf).is_ok() && buf.iter().any(|b| *b != 0) {
                        nz += 1;
                        let l = (p * 4096 / rec.layout.layer_size.max(1)) as usize;
                        if let Some(c) = per_layer.get_mut(l) {
                            *c += 1;
                        }
                    }
                }
            }
            eprintln!("DIAG hmsrc per layer pages with data {per_layer:?}");
            eprintln!(
                "DIAG hmsrc draw {n} fs {:?} src {:?} {:#x} {:?} {}x{}x{} residency {:?} guest pages with data {nz}/{pages} tic base_layer {} layers {} base_level {}",
                d.program.fragment, slot.texture, rec.address.get(), rec.desc.format, rec.desc.width, rec.desc.height, rec.desc.depth,
                rec.residency, slot.header.view.base_layer, slot.header.view.layers, slot.header.view.base_level
            );
        }
    }
    if crate::diag("hm") && HM_PASS.with(std::cell::Cell::get) {
        static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        if SHOWN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 60 {
            let tex: Vec<_> = d
                .textures
                .iter()
                .filter(|t| t.handle != 0)
                .map(|t| {
                    let info = sh.table.resolve(t.handle).ok().map(|(slot, _)| {
                        let res = sh.textures.get(&slot.texture).map(|r| (r.desc.kind, r.desc.width, r.desc.height, r.desc.array_layers, r.desc.format));
                        let rec = sh.table.record(slot.texture).ok().map(|r| r.residency);
                        (slot.texture, res, rec)
                    });
                    (t.stage, t.slot, info)
                })
                .collect();
            eprintln!(
                "DIAG hm draw {n} size {:?} vs {} fs {:?} args {:?} masks0 {} blend0 {:?} depth {}/{} tex {tex:?}",
                pc.size, d.program.vertex, d.program.fragment, d.args, d.write_masks[0],
                d.blend.first().map(|b| (b.enable, b.src_rgb, b.dst_rgb)), d.depth_stencil.depth_test, d.depth_stencil.depth_func
            );
        }
    }
    if crate::diag("terr") {
        static KEYS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
        let keys = KEYS.get_or_init(|| {
            std::fs::read_to_string("/tmp/fmap/terrain_keys.txt")
                .map(|t| t.lines().map(str::to_owned).collect())
                .unwrap_or_default()
        });
        if keys.iter().any(|k| d.program.vertex.starts_with(k.as_str())) {
            static SEEN: std::sync::Mutex<Vec<(String, u64)>> = std::sync::Mutex::new(Vec::new());
            for t in d.textures.iter().filter(|t| t.handle != 0) {
                let key = (format!("{}{:?}{}", d.program.vertex, t.stage, t.slot), t.handle);
                let Ok(mut seen) = SEEN.lock() else { break };
                if seen.contains(&key) || seen.len() > 400 {
                    continue;
                }
                seen.push(key);
                let info = sh.table.resolve(t.handle).ok().map(|(slot, _)| {
                    let res = sh.textures.get(&slot.texture).map(|r| {
                        (r.desc.kind, r.desc.width, r.desc.height, r.desc.array_layers, r.desc.mip_levels, r.desc.format, r.scaled)
                    });
                    let rec = sh.table.record(slot.texture).ok().map(|r| (r.residency, r.desc.target, r.desc.depth));
                    (slot.texture, slot.header.target, slot.header.view.target, slot.header.view.base_layer, slot.header.view.layers, res, rec)
                });
                eprintln!(
                    "DIAG terr vs {} fs {:?} {:?} slot {} handle {:#x} fit {:?} -> {:?}",
                    &d.program.vertex[..16.min(d.program.vertex.len())],
                    d.program.fragment.as_deref().map(|f| &f[..16.min(f.len())]),
                    t.stage, t.slot, t.handle,
                    sh.env.handles.get(&t.handle).map(|r| r.texture != 0),
                    info
                );
            }
        }
    }
    if crate::diag("ubo") && crate::lum::active() && pc.size == [1600, 900] && n >= 2168 {
        static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        for u in d.ubos.iter().filter(|u| u.snapshot.is_some() || u.size >= 4096) {
            if SHOWN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= 400 {
                break;
            }
            let mut live = vec![0u8; (u.size as usize).min(1 << 16)];
            let read = match sh.guest_offset(u.address, live.len() as u64) {
                Some(off) => sh.guest.read(off, &mut live).is_ok(),
                None => false,
            };
            let nz_live = if read { live.iter().filter(|b| **b != 0).count() } else { usize::MAX };
            let nz_snap = u.snapshot.as_ref().map_or(usize::MAX, |b| b.iter().filter(|x| **x != 0).count());
            let head: Vec<f32> = u
                .snapshot
                .as_ref()
                .map(|b| b.chunks_exact(4).take(8).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
                .unwrap_or_default();
            eprintln!(
                "DIAG ubo draw {n} {:?} slot {} size {} snapshot {:?} nz_snap {nz_snap} nz_live {nz_live} head {head:?}",
                u.stage, u.slot, u.size, u.snapshot.as_ref().map(|b| b.len())
            );
        }
    }
    if crate::diag("geom") && crate::lum::active() && pc.size == [1600, 900] && n % 150 == 0 {
        let floats = |addr: u64, count: usize| -> Vec<f32> {
            let mut bytes = vec![0u8; count * 4];
            match sh.guest_offset(addr, bytes.len() as u64) {
                Some(off) if sh.guest.read(off, &mut bytes).is_ok() => {
                    bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
                }
                _ => Vec::new(),
            }
        };
        if let Some(s0) = d.streams.first() {
            eprintln!("DIAG geom draw {n} stream0 {:?} pos {:?}", d.attributes.first(), floats(s0.address, 9));
        }
        for u in d.ubos.iter().filter(|u| u.slot <= 2 && matches!(u.stage, crate::detail::Stage::Vertex)) {
            let n_floats = 24.min(u.size as usize / 4);
            let live = floats(u.address, n_floats);
            let snap: Vec<f32> = u
                .snapshot
                .as_ref()
                .map(|b| b.chunks_exact(4).take(n_floats).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
                .unwrap_or_default();
            eprintln!("DIAG geom draw {n} ubo{} size {} live {live:?} snapshot {snap:?}", u.slot, u.size);
        }
        eprintln!("DIAG geom draw {n} viewport {:?} depth_range {:?} scissor {:?}", d.dynamic.viewport, d.dynamic.depth_range, d.dynamic.scissor);
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
            let key = (format!("{:?}", d.program.fragment), format!("{unresolved:?}"));
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
    let p = prepare(sh, pc, d, &mut ScratchSink::new(scratch, sh.uploaded, Some(memo)), false);
    if crate::diag("pixel") && pc.size == [1920, 1080] {
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
    let p = match p {
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
    if crate::diag("hm") || crate::diag("hmsrc") || crate::diag("hmcap") {
        let hm = sh.plan.attachments_of(pass).iter().any(|a| {
            sh.textures.get(&a.tex).is_some_and(|r| {
                r.desc.kind == nxkit_metal::TextureKind::D2Array && r.desc.array_layers == 8 && r.desc.width >= 256
            })
        });
        HM_PASS.with(|c| c.set(hm));
    }
    let lost =
        |stats: &mut EncodeStats, why: Skip| stats.skipped[why as usize] += range.len() as u32;
    if crate::diag("barr") && crate::lum::active() {
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
    let mut memo = ArgsMemo::default();
    let mut n = range.start;
    while n < range.end {
        if let Some(run) = sh.run_at.get(&n).filter(|r| n + r.draws <= range.end) {
            if run_icb(sh, &s.pc, &mut rp, &mut tr, run, stats) {
                n += run.draws;
                continue;
            }
        }
        draw(sh, &s.pc, &mut rp, &mut tr, scratch, &mut memo, n, stats);
        n += 1;
    }
    rp.end();
    if crate::lum::active() || (crate::diag("hmcap") && HM_PASS.with(std::cell::Cell::get)) {
        for a in sh.plan.attachments_of(pass) {
            if matches!(a.slot, Slot::Color(_)) && a.store == StoreAction::Store {
                if let Slot::Color(slot) = a.slot {
                    let Some(Some(color)) = s.desc.colors.get(usize::from(slot)) else { continue };
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
    use nxkit_metal::{ArchiveConfig, Format, FrameRing, PipelineCache, Primitive,
        RenderPipelineDesc, ShaderLibrary, Texture, TextureDesc};

    #[test]
    fn inactive_aliases_leave_the_original_output_locations_empty() {
        let colors = [(0, TexId(1), false), (1, TexId(2), false),
            (2, TexId(2), false), (3, TexId(3), false), (4, TexId(3), false)];
        assert_eq!(distinct_color_slots(&colors, 0b01011).unwrap(), 0b01011);
        assert_eq!(distinct_color_slots(&colors, 0b10101).unwrap(), 0b10101);
        assert_eq!(distinct_color_slots(&[(1, TexId(2), false), (2, TexId(2), true)], 0).unwrap(), 4);
        assert!(distinct_color_slots(&colors, 0b00110).is_err());
    }

    #[test]
    fn an_inactive_alias_cannot_overwrite_a_rendered_color() {
        let Ok(ctx) = Context::system_default() else { return };
        let selected = distinct_color_slots(&[(0, TexId(1), false), (1, TexId(1), false)], 1).unwrap();
        let lib = ShaderLibrary::from_source(&ctx, "alias-output", r#"
#include <metal_stdlib>
using namespace metal;
struct V { float4 pos [[position]]; };
vertex V vs(uint id [[vertex_id]]) {
    float2 p = float2((id << 1) & 2, id & 2);
    return {float4(p * 2 - 1, 0, 1)};
}
struct Outputs { float4 a [[color(0)]]; float4 b [[color(1)]]; };
fragment Outputs fs() { return {float4(1,0,0,1), float4(0)}; }
"#).unwrap();
        let mut texture_desc = TextureDesc::render_target_2d(16, 16, Format::Rgba8Unorm);
        texture_desc.storage = Storage::Shared;
        let texture = Texture::new(&ctx, texture_desc).unwrap();
        let mut pipeline_desc = RenderPipelineDesc::new((&lib, "vs"), Some((&lib, "fs")), &[Format::Rgba8Unorm; 2]);
        pipeline_desc.colors[1].as_mut().unwrap().write_mask = 0;
        let mut colors = vec![Some(ColorAttachment { depth_plane: 0, texture: texture.clone(), load: MLoad::Clear,
            store: MStore::Store, clear: [0.0; 4], memoryless: false }); 2];
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
        let mut pass = frame.commands().begin_render_pass(&RenderPassDesc { colors, depth: None, stencil: None }, &table).unwrap();
        pass.encode(&RenderCommand::SetPipeline(pipeline)).unwrap();
        pass.encode(&RenderCommand::Draw { primitive: Primitive::Triangle, first_vertex: 0,
            vertex_count: 3, instance_count: 1, base_instance: 0 }).unwrap();
        pass.end();
        frame.submit().unwrap().wait().unwrap();
        let mut pixels = vec![0; 16 * 16 * 4];
        texture.read(16 * 4, &mut pixels).unwrap();
        assert!(pixels.chunks_exact(4).all(|p| p == [255, 0, 0, 255]));
    }
}
