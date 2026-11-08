//! Metal textures of the resource table.
//!
//! The store makes one Metal texture per `TexId` and keeps it while the description stays the same. It decides the
//! render target scale (R19), the memoryless storage of tile-only attachments, and the views for descriptors.
//!
//! How a texture gets the scaled size: a texture is scaled when the scale is not 1, a render pass wrote it in this
//! frame or an earlier one (a texture that was an attachment once stays one), and no CPU read of it has been seen. The
//! game creates its output-size targets and renders into them, so they become attachments and with that scaled. A
//! texture that the game only uploads is never an attachment and keeps its size. A texture that the CPU reads back
//! stays at its size for good, because the guest memory has the unscaled bytes.

use crate::draws::{FastMap, FastSet};
use std::sync::{Mutex, PoisonError};

use nxkit_metal::{Context, Format, Storage, Texture, TextureDesc, TextureKind, ViewDesc};
use nxkit_nvn::format::{Aspects, format_info};
use nxkit_nvn::layout::Target;
use nxkit_nvn::resource::{Residency, TexId, TextureRecord};
use nxkit_nvn::tic::Tic;

use crate::format_map::{Mapped, texture_format};
use crate::stats::{EncodeStats, Skip};

/// A texture of the store as the encoder sees it.
#[derive(Clone)]
pub(crate) struct Res {
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
}

type ViewKey = (TexId, u64, [u32; 6]);

/// The textures by id.
pub(crate) struct TextureStore {
    ctx: Context,
    entries: HashMap<TexId, Res>,
    attachments: HashSet<TexId>,
    pinned: HashSet<TexId>,
    next_generation: u64,
    views: Mutex<HashMap<ViewKey, Texture>>,
    retired: Vec<Texture>,
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

/// The Metal description of a record at `scale` (1.0 for an unscaled texture).
pub(crate) fn describe(rec: &TextureRecord, scale: f32, u: Use) -> Result<(TextureDesc, bool), Skip> {
    let d = &rec.desc;
    let mapped = texture_format(d.format).ok_or(Skip::Format)?;
    let kind = kind_of(d.target).ok_or(Skip::Format)?;
    let format = mapped.format();
    let info = format_info(d.format).map_err(|_| Skip::Format)?;
    let depth_like = matches!(info.aspects, Aspects::Depth | Aspects::Stencil | Aspects::DepthStencil);
    let (width, height, depth, layers) = match d.target {
        Target::D1 => (d.width, 1, 1, 1),
        Target::D1Array => (d.width, 1, 1, d.height.max(1)),
        Target::D2 | Target::Rectangle | Target::D2Multisample | Target::Cubemap => (d.width, d.height, 1, 1),
        Target::D2Array | Target::D2MultisampleArray => (d.width, d.height, 1, d.depth.max(1)),
        Target::D3 => (d.width, d.height, d.depth.max(1), 1),
        Target::CubemapArray => (d.width, d.height, 1, (d.depth / 6).max(1)),
        Target::Buffer => return Err(Skip::Format),
    };
    let (width, height) = (scaled_size(width, scale), scaled_size(height, scale));
    let max_levels = 32 - width.max(height).max(depth).leading_zeros();
    let render_target = u.attachment;
    let memoryless = u.memoryless && render_target && !u.cpu_read;
    let compressed = matches!(format, Format::Bc(_) | Format::Astc { .. });
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
        storage: if memoryless { Storage::Memoryless } else { Storage::Private },
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
            next_generation: 1,
            views: Mutex::new(FastMap::default()),
            retired: Vec::new(),
            spans: Mutex::new(Vec::new()),
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
    }

    /// True when the texture is scaled at `scale`.
    pub(crate) fn is_scaled(&self, id: TexId, scale: f32) -> bool {
        scale != 1.0 && self.attachments.contains(&id) && !self.pinned.contains(&id)
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
        u.attachment |= self.attachments.contains(&id);
        let scaled = self.is_scaled(id, scale);
        let (desc, widened) = describe(rec, if scaled { scale } else { 1.0 }, u)?;
        if let Some(r) = self.entries.get_mut(&id) {
            if r.desc == desc {
                r.fresh = false;
                return Ok(r.clone());
            }
            if r.scaled != scaled {
                stats.textures_rescaled += 1;
            }
        }
        let tex = Texture::new(&self.ctx, desc).map_err(|_| Skip::Texture)?;
        let generation = self.next_generation;
        self.next_generation += 1;
        let r = Res { tex, desc, generation, scaled, fresh: true, widened };
        stats.textures_created += 1;
        if let Some(old) = self.entries.insert(id, r.clone()) {
            self.retired.push(old.tex);
        }
        Ok(r)
    }

    /// Forgets the Metal texture of `id` (the game created or finalized the texture again).
    pub(crate) fn invalidate(&mut self, id: TexId) {
        if crate::diag("arr") {
            if let Some(r) = self.entries.get(&id) {
                if r.desc.array_layers == 8 && r.desc.width >= 256 {
                    eprintln!("DIAG arr invalidate {id:?} gen {} {:?} {}x{}", r.generation, r.desc.format, r.desc.width, r.desc.height);
                }
            }
        }
        if let Some(old) = self.entries.remove(&id) {
            self.retired.push(old.tex);
        }
        self.attachments.remove(&id);
        self.pinned.remove(&id);
    }

    /// The textures that were replaced. The caller drops them when the GPU is done with them.
    pub(crate) fn take_retired(&mut self) -> Vec<Texture> {
        std::mem::take(&mut self.retired)
    }

    /// The Metal texture that a sampler descriptor sees: the texture itself, or a view for the levels, layers and
    /// format of the descriptor. The channel swizzle of the descriptor has no Metal view equivalent here.
    pub(crate) fn view(&self, id: TexId, res: &Res, tic: &Tic, stats: &mut EncodeStats) -> Result<Texture, Skip> {
        if tic.view.swizzle != [2, 3, 4, 5] {
            stats.swizzle_ignored += 1;
        }
        let d = res.desc;
        let levels = if tic.view.levels == 0 { d.mip_levels } else { u32::from(tic.view.levels) };
        let first_level = u32::from(tic.view.base_level).min(d.mip_levels - 1);
        let level_count = levels.min(d.mip_levels - first_level).max(1);
        let cube = matches!(d.kind, TextureKind::Cube | TextureKind::CubeArray);
        // The layer range of a cube view is not mapped. The view shows all cubes.
        let slices = if cube { d.slice_count() / 6 } else { d.slice_count() };
        let (first_slice, slice_count) = if cube {
            (0, slices)
        } else {
            let first = u32::from(tic.view.base_layer).min(slices - 1);
            let count = if tic.view.layers == 0 { slices } else { u32::from(tic.view.layers) };
            (first, count.min(slices - first).max(1))
        };
        let format = match tic.view.format.and_then(texture_format) {
            Some(Mapped::Exact(f))
                if f.block_bytes() == d.format.block_bytes()
                    && f.is_compressed() == d.format.is_compressed()
                    && !res.widened =>
            {
                f
            }
            _ => d.format,
        };
        // The shader declares a 2D or a 2D array texture by the target of the descriptor. A one layer view of an array
        // is 2D, and a texture without layers can be an array of one.
        let mut kind = d.kind;
        let mut slice_count = slice_count;
        if matches!(d.kind, TextureKind::D2 | TextureKind::D2Array) {
            match kind_of(tic.view.target.unwrap_or(tic.target)) {
                Some(TextureKind::D2) => {
                    kind = TextureKind::D2;
                    slice_count = 1;
                }
                Some(TextureKind::D2Array) => kind = TextureKind::D2Array,
                _ => {}
            }
        }
        let identity = first_level == 0
            && level_count == d.mip_levels
            && first_slice == 0
            && slice_count == slices
            && format == d.format
            && kind == d.kind;
        if identity || d.storage == Storage::Memoryless {
            return Ok(res.tex.clone());
        }
        let key: ViewKey = (
            id,
            res.generation,
            [first_level, level_count, first_slice, slice_count, format_key(format), kind as u32],
        );
        let mut views = self.views.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(v) = views.get(&key) {
            return Ok(v.clone());
        }
        let v = res
            .tex
            .view(&ViewDesc { format, kind, first_level, level_count, first_slice, slice_count })
            .map_err(|_| Skip::Texture)?;
        views.insert(key, v.clone());
        Ok(v)
    }

    /// Drops the views of replaced textures.
    pub(crate) fn trim_views(&self) {
        let live: HashMap<TexId, u64> = self.entries.iter().map(|(k, v)| (*k, v.generation)).collect();
        self.views
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(id, g, _), _| live.get(id) == Some(g));
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
