//! Metal textures of the resource table.
//!
//! The store makes one Metal texture per `TexId` and keeps it while the description stays the same. It decides the
//! render target scale (R19), the memoryless storage of tile-only attachments, and the views for descriptors.
//!
//! How a texture gets the scaled size: a texture is scaled when the scale is not 1, a render pass wrote it in this
//! frame or an earlier one (a texture that was an attachment once stays one), and no CPU read of it has been seen. The
//! game creates its output-size targets and renders into them, so they become attachments and with that scaled. A
//! texture that the game only uploads is never an attachment and keeps its size. A texture that the CPU reads back
//! stays at its size for good, because the guest memory has the unscaled bytes. So does a texture that the CPU writes
//! often and the GPU renders rarely (`Heat`): every write would need a staging upload and a scale.

mod blocks;
mod evict;
pub mod inventory;
mod supersede;
#[cfg(test)]
mod tests;
mod views;

use crate::draws::{FastMap, FastSet};
use std::sync::{Mutex, PoisonError};

use nxkit_metal::{
    Context, Format, Storage, Texture, TextureDesc, TextureKind, TextureRegion, ViewDesc,
};

pub(crate) use self::blocks::BlockTarget;
use nxkit_nvn::format::{Aspects, format_info};
use nxkit_nvn::layout::Target;
use nxkit_nvn::resource::{Residency, TexId, TextureRecord};
use nxkit_nvn::tic::Tic;

use crate::format_map::{Mapped, texture_format};
use crate::stats::{EncodeStats, Skip};

/// A texture of the store as the encoder sees it.
#[derive(Clone)]
pub(crate) struct Res {
    /// The stable hazard key of this backing texture, including its aliases.
    pub(crate) barrier_id: TexId,
    /// The Metal texture.
    pub(crate) tex: Texture,
    /// The description it was made with.
    pub(crate) desc: TextureDesc,
    /// Grows with each new Metal texture of the id. ICBs compare it.
    pub(crate) generation: u64,
    /// The size is the NVN size times the scale.
    pub(crate) scaled: bool,
    /// The Metal texture is new since the previous frame, so it holds no data.
    pub(crate) fresh: bool,
    /// The Metal format has another layout than the NVN format (depth 24 bits).
    pub(crate) widened: bool,
    /// The GPU address of the guest storage this texture was made for (the memory report keeps it; 0 for a staging
    /// texture).
    pub(crate) guest_address: u64,
    /// The bytes of that storage in guest memory.
    pub(crate) guest_size: u64,
    /// The scale of the size when it was made: 1 for an unscaled texture.
    pub(crate) scale: f32,
}

/// How a texture is used in this frame.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Use {
    /// A render pass has it as an attachment.
    pub(crate) attachment: bool,
    /// All its attachment uses in the frame are memoryless.
    pub(crate) memoryless: bool,
    /// The CPU reads it.
    pub(crate) cpu_read: bool,
    /// A shader runs image atomics on it (an `img` binding with `atomic`). It stays set for the texture.
    pub(crate) atomic: bool,
}

type ViewKey = (u64, [u32; 7]);

/// How often the CPU writes a texture and the GPU renders into it, in frames that use the texture.
#[derive(Clone, Copy, Debug, Default)]
struct Heat {
    /// Frames in which the guest copy was newer.
    writes: u32,
    /// Frames in which a render pass had it as an attachment.
    renders: u32,
}

/// The CPU writes a texture in at least this many frames before it can stay native.
const HEAT_MIN_WRITES: u32 = 8;
/// ... and in at least this many times as many frames as the GPU renders into it.
const HEAT_RATIO: u32 = 4;

impl Heat {
    /// True for a texture that is written much more often than it is rendered: scaling it costs an upload and a scale
    /// per write and buys little.
    fn mostly_cpu(self) -> bool {
        self.writes >= HEAT_MIN_WRITES && self.writes / HEAT_RATIO >= self.renders
    }
}

/// The textures by id.
pub(crate) struct TextureStore {
    ctx: Context,
    entries: FastMap<TexId, Res>,
    aliases: FastMap<(u64, TextureDesc), Res>,
    attachments: FastSet<TexId>,
    pinned: FastSet<TexId>,
    /// The textures that image atomics ran on, which are made with `shader_atomic` from then on.
    atomic: FastSet<TexId>,
    /// The CPU-written, rarely rendered textures, which stay at the native size for good.
    native: FastSet<TexId>,
    heat: FastMap<TexId, Heat>,
    next_generation: u64,
    views: Mutex<FastMap<ViewKey, Texture>>,
    /// The block shadows of compressed attachments by generation, level, layer and view format (`blocks`).
    shadows: Mutex<FastMap<(u64, u32, u32, u32), Texture>>,
    retired: Vec<Texture>,
    /// The storage ranges of the textures made so far, for the `contain` diagnostic.
    spans: Mutex<Vec<(u64, u64, TexId, TextureDesc)>>,
    /// The frames the store has seen end.
    frame: u64,
    /// The idle frames after which a texture goes, `None` when eviction is off (`NXKIT_TEX_EVICT`).
    evict_after: Option<u64>,
    /// The use of each Metal texture by generation.
    usage: FastMap<u64, evict::Usage>,
    /// The ids whose Metal texture was evicted and not made again yet.
    evicted_ids: FastSet<TexId>,
    /// The memory report is on, so every scan publishes the inventory.
    inventory: bool,
    /// The textures dropped because another texture overwrote their memory (`supersede.rs`).
    sup: supersede::State,
}

fn kind_of(t: Target) -> Option<TextureKind> {
    Some(match t {
        Target::D1 | Target::D2 | Target::Rectangle | Target::D2Multisample => TextureKind::D2,
        Target::D1Array | Target::D2Array | Target::D2MultisampleArray => TextureKind::D2Array,
        Target::D3 => TextureKind::D3,
        Target::Cubemap => TextureKind::Cube,
        Target::CubemapArray => TextureKind::CubeArray,
        Target::Buffer => return None,
    })
}

fn scaled_size(v: u32, scale: f32) -> u32 {
    ((v as f32 * scale).round() as u32).max(1)
}

/// Whether the `life` category prints the creation or the end of a Metal texture of this description: it has to pass
/// `NXKIT_DIAG_TEX`, and at most 300 lines are printed.
fn life_diag(format: Format, width: u32, height: u32, layers: u32) -> bool {
    static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    crate::diag("life")
        && crate::diag::tex_match(format, width, height, layers)
        && crate::diag::first(&SHOWN, 300)
}

/// The Metal description of a record at `scale` (1.0 for an unscaled texture).
pub(crate) fn describe(
    rec: &TextureRecord,
    scale: f32,
    u: Use,
) -> Result<(TextureDesc, bool), Skip> {
    let d = &rec.desc;
    let unsupported = |what: &str| {
        nxkit_nvn::diag::note(
            nxkit_nvn::diag::Reason::FormatUnsupported,
            format_args!(
                "{what} {:?} {:?} {}x{}x{}",
                d.format, d.target, d.width, d.height, d.depth
            ),
        );
        Skip::Format
    };
    let mapped = texture_format(d.format).ok_or_else(|| unsupported("format"))?;
    // Cube textures are 2D arrays of their faces, so that a shader may read them as a cube, a cube array or a 2D
    // array: Metal makes a cube array view only of a 2D array.
    let kind = match kind_of(d.target).ok_or_else(|| unsupported("target"))? {
        TextureKind::Cube | TextureKind::CubeArray => TextureKind::D2Array,
        k => k,
    };
    let format = mapped.format();
    let info = format_info(d.format).map_err(|_| Skip::Format)?;
    let depth_like = matches!(
        info.aspects,
        Aspects::Depth | Aspects::Stencil | Aspects::DepthStencil
    );
    let (width, height, depth, layers) = match d.target {
        Target::D1 => (d.width, 1, 1, 1),
        Target::D1Array => (d.width, 1, 1, d.height.max(1)),
        Target::D2 | Target::Rectangle | Target::D2Multisample => (d.width, d.height, 1, 1),
        Target::Cubemap => (d.width, d.height, 1, 6),
        Target::D2Array | Target::D2MultisampleArray => (d.width, d.height, 1, d.depth.max(1)),
        Target::D3 => (d.width, d.height, d.depth.max(1), 1),
        Target::CubemapArray => (d.width, d.height, 1, (d.depth / 6).max(1) * 6),
        Target::Buffer => return Err(unsupported("target")),
    };
    let (width, height) = (scaled_size(width, scale), scaled_size(height, scale));
    let max_levels = 32 - width.max(height).max(depth).leading_zeros();
    let compressed = matches!(format, Format::Bc(_) | Format::Astc { .. });
    // A texture that the frame only samples can share storage with one that another frame renders into: the game
    // makes texture objects for the same memory again, and the store shares a Metal texture between records only
    // when their descriptions match. So every texture that Metal can render into gets the usage, and a record that
    // is sampled first sees what a later render target of the same memory wrote.
    // Metal cannot render into a compressed format: an attachment of one renders into a block shadow (`blocks`).
    let render_target = !compressed && (u.attachment || format != Format::Rgb9E5Float);
    let memoryless = u.memoryless && u.attachment && !u.cpu_read;
    let exact = matches!(mapped, Mapped::Exact(_));
    let desc = TextureDesc {
        kind,
        width,
        height,
        depth,
        mip_levels: d.levels.max(1).min(max_levels),
        array_layers: layers,
        allow_views: true,
        format,
        shader_read: !memoryless,
        shader_write: exact && !depth_like && !compressed && !memoryless,
        render_target,
        storage: if memoryless {
            Storage::Memoryless
        } else {
            Storage::Private
        },
        shader_atomic: u.atomic && exact && !depth_like && !compressed && !memoryless,
    };
    Ok((desc, !exact))
}

impl TextureStore {
    pub(crate) fn new(ctx: &Context) -> Self {
        Self {
            ctx: ctx.clone(),
            entries: FastMap::default(),
            aliases: FastMap::default(),
            attachments: FastSet::default(),
            pinned: FastSet::default(),
            atomic: FastSet::default(),
            native: FastSet::default(),
            heat: FastMap::default(),
            next_generation: 1,
            views: Mutex::new(FastMap::default()),
            shadows: Mutex::new(FastMap::default()),
            retired: Vec::new(),
            spans: Mutex::new(Vec::new()),
            frame: 0,
            evict_after: evict::frames_from_env(),
            usage: FastMap::default(),
            evicted_ids: FastSet::default(),
            inventory: inventory::wanted(),
            sup: supersede::State::from_env(),
        }
    }

    /// The texture of an id, if the store made one.
    pub(crate) fn get(&self, id: TexId) -> Option<&Res> {
        self.entries.get(&id)
    }

    /// Notes how the frame uses a texture. Call it for every texture before `ensure`.
    pub(crate) fn note(&mut self, id: TexId, u: Use) {
        if u.attachment {
            self.attachments.insert(id);
        }
        if u.cpu_read {
            self.pinned.insert(id);
        }
        if u.atomic {
            self.atomic.insert(id);
        }
    }

    /// True when the texture is scaled at `scale`.
    pub(crate) fn is_scaled(&self, id: TexId, scale: f32) -> bool {
        scale != 1.0
            && self.attachments.contains(&id)
            && !self.pinned.contains(&id)
            && !self.native.contains(&id)
    }

    /// The texture of `id` in the shape the frame needs. A texture with another description is made again.
    pub(crate) fn ensure(
        &mut self,
        rec: &TextureRecord,
        scale: f32,
        mut u: Use,
        stats: &mut EncodeStats,
    ) -> Result<Res, Skip> {
        let id = rec.id;
        if scale != 1.0 && !self.native.contains(&id) {
            let h = self.heat.entry(id).or_default();
            h.writes += u32::from(rec.residency == Residency::GuestNewer);
            h.renders += u32::from(u.attachment);
            if h.mostly_cpu() {
                self.native.insert(id);
                stats.textures_kept_native += 1;
            }
        }
        u.attachment |= self.attachments.contains(&id);
        u.atomic |= self.atomic.contains(&id);
        // A compressed texture keeps its size: its attachments render blocks, which a scale would not keep.
        let compressed =
            texture_format(rec.desc.format).is_some_and(|m| m.format().is_compressed());
        let scaled = !compressed && self.is_scaled(id, scale);
        let (desc, widened) = describe(rec, if scaled { scale } else { 1.0 }, u)?;
        if let Some(r) = self.entries.get_mut(&id) {
            if r.desc == desc {
                r.fresh = false;
                let r = r.clone();
                self.used(r.generation, rec.residency == Residency::GpuNewer);
                return Ok(r);
            }
            if r.scaled != scaled {
                stats.textures_rescaled += 1;
            }
        }
        let key = (rec.address.get(), desc);
        let r = if let Some(alias) = self.aliases.get(&key) {
            Res {
                fresh: false,
                ..alias.clone()
            }
        } else {
            static CONTAIN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if crate::diag("contain") && CONTAIN.load(std::sync::atomic::Ordering::Relaxed) < 300 {
                let (a0, a1) = (rec.address.get(), rec.address.get() + rec.layout.size);
                let mut spans = self.spans.lock().unwrap_or_else(PoisonError::into_inner);
                for &(b0, b1, other, od) in spans.iter() {
                    let inside = a0 >= b0 && a1 <= b1 && (a0, a1) != (b0, b1);
                    let around = b0 >= a0 && b1 <= a1 && (a0, a1) != (b0, b1);
                    if (inside || around) && other != id && crate::diag::first(&CONTAIN, 300) {
                        eprintln!(
                            "DIAG contain {} new {id:?} {:?} {}x{}x{} l{} {:?} [{a0:#x}+{:#x}] {} old {other:?} {:?} {}x{}x{} l{} {:?} [{b0:#x}+{:#x}] off {:#x}",
                            if inside { "inside" } else { "around" },
                            desc.kind,
                            desc.width,
                            desc.height,
                            desc.array_layers,
                            desc.mip_levels,
                            desc.format,
                            a1 - a0,
                            if inside { "of" } else { "holds" },
                            od.kind,
                            od.width,
                            od.height,
                            od.array_layers,
                            od.mip_levels,
                            od.format,
                            b1 - b0,
                            a0.abs_diff(b0)
                        );
                    }
                }
                if spans.len() < 20000 {
                    spans.push((a0, a1, id, desc));
                }
            }
            static SPLIT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if crate::diag("split") {
                for ((a, other), r) in &self.aliases {
                    if *a == rec.address.get() && crate::diag::first(&SPLIT, 300) {
                        eprintln!(
                            "DIAG split addr {a:#x} new {id:?} {:?} {}x{}x{} l{} {:?} rt {} | old {:?} {:?} {}x{}x{} l{} {:?} rt {}",
                            desc.kind,
                            desc.width,
                            desc.height,
                            desc.array_layers,
                            desc.mip_levels,
                            desc.format,
                            desc.render_target,
                            r.barrier_id,
                            other.kind,
                            other.width,
                            other.height,
                            other.array_layers,
                            other.mip_levels,
                            other.format,
                            other.render_target
                        );
                    }
                }
            }
            if life_diag(desc.format, desc.width, desc.height, desc.array_layers) {
                eprintln!(
                    "DIAG life new metal texture {id:?} at {:#x} {:?} {}x{} gen {} (entry had {:?})",
                    rec.address.get(),
                    desc.format,
                    desc.width,
                    desc.height,
                    self.next_generation,
                    self.entries.get(&id).map(|r| r.generation)
                );
            }
            let t_create = std::time::Instant::now();
            let made = Texture::new(&self.ctx, desc);
            stats.texture_create_ns += u64::try_from(t_create.elapsed().as_nanos()).unwrap_or(0);
            let tex = made.map_err(|e| {
                let what = format_args!("new {id:?} {desc:?}: {e:?}");
                nxkit_nvn::diag::note(nxkit_nvn::diag::Reason::TextureCreate, what);
                crate::tex_skip(what)
            })?;
            let generation = self.next_generation;
            self.next_generation += 1;
            let r = Res {
                barrier_id: id,
                tex,
                desc,
                generation,
                scaled,
                fresh: true,
                widened,
                guest_address: rec.address.get(),
                guest_size: rec.layout.size,
                scale: if scaled { scale } else { 1.0 },
            };
            self.aliases.insert(key, r.clone());
            stats.textures_created += 1;
            r
        };
        if let Some(old) = self.entries.insert(id, r.clone()) {
            self.retired.push(old.tex);
        }
        self.used(r.generation, rec.residency == Residency::GpuNewer);
        self.made_again(id);
        self.remade_after_supersede(
            id,
            (r.desc.format, r.desc.width, r.desc.height),
            rec.residency == Residency::GpuNewer,
        );
        Ok(r)
    }

    /// Forgets the Metal texture of `id` (the game created or finalized the texture again).
    pub(crate) fn invalidate(&mut self, id: TexId) {
        if let Some(r) = self.entries.get(&id)
            && life_diag(
                r.desc.format,
                r.desc.width,
                r.desc.height,
                r.desc.array_layers,
            )
        {
            eprintln!(
                "DIAG life invalidate {id:?} gen {} {:?} {}x{}",
                r.generation, r.desc.format, r.desc.width, r.desc.height
            );
        }
        if let Some(old) = self.entries.remove(&id) {
            self.retired.push(old.tex);
        }
        self.attachments.remove(&id);
        self.pinned.remove(&id);
        self.atomic.remove(&id);
        self.native.remove(&id);
        self.heat.remove(&id);
        self.evicted_ids.remove(&id);
        self.sup.forget(id);
    }

    /// The textures that were replaced. The caller drops them when the GPU is done with them.
    pub(crate) fn take_retired(&mut self) -> Vec<Texture> {
        std::mem::take(&mut self.retired)
    }

    /// True when the guest bytes of a scaled texture have to go into it through a staging texture of the native size
    /// and a GPU scale. Only a CPU write counts. A texture that became scaled this frame is new and empty either way.
    pub(crate) fn needs_scaled_upload(res: &Res, rec: &TextureRecord) -> bool {
        res.scaled
            && !res.widened
            && res.desc.storage != Storage::Memoryless
            && rec.residency == Residency::GuestNewer
    }

    /// True when the guest bytes have to go into the Metal texture before the frame uses it.
    pub(crate) fn needs_upload(res: &Res, rec: &TextureRecord) -> bool {
        !res.scaled
            && !res.widened
            && res.desc.storage != Storage::Memoryless
            && (rec.residency == Residency::GuestNewer
                || (res.fresh && rec.residency == Residency::InSync))
    }
}

fn format_key(f: Format) -> u32 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut h = DefaultHasher::new();
    f.hash(&mut h);
    h.finish() as u32
}
