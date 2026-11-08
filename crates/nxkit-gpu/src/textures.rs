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

type ViewKey = (u64, [u32; 7]);

/// The textures by id.
pub(crate) struct TextureStore {
    ctx: Context,
    entries: FastMap<TexId, Res>,
    aliases: FastMap<(u64, TextureDesc), Res>,
    attachments: FastSet<TexId>,
    pinned: FastSet<TexId>,
    next_generation: u64,
    views: Mutex<FastMap<ViewKey, Texture>>,
    retired: Vec<Texture>,
    /// The storage ranges of the textures made so far, for the `contain` diagnostic.
    spans: Mutex<Vec<(u64, u64, TexId, TextureDesc)>>,
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
pub(crate) fn describe(
    rec: &TextureRecord,
    scale: f32,
    u: Use,
) -> Result<(TextureDesc, bool), Skip> {
    let d = &rec.desc;
    let mapped = texture_format(d.format).ok_or(Skip::Format)?;
    // Cube textures are 2D arrays of their faces, so that a shader may read them as a cube, a cube array or a 2D
    // array: Metal makes a cube array view only of a 2D array.
    let kind = match kind_of(d.target).ok_or(Skip::Format)? {
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
    /// A single renderable mip/layer. Sampling views and attachment views share the cache.
    pub(crate) fn target_view(&self, res: &Res, view: nxkit_nvn::frame::TargetView) -> Result<Texture, Skip> {
        let d = res.desc;
        let format = match view.format().and_then(texture_format) {
            Some(Mapped::Exact(format)) if !res.widened => format,
            _ => d.format,
        };
        if d.kind == TextureKind::D2 && d.mip_levels == 1 && view.level() == 0 && view.layer() == 0 && format == d.format {
            return Ok(res.tex.clone());
        }
        let kind = if d.kind == TextureKind::D3 { TextureKind::D3 } else { TextureKind::D2 };
        let slice = if kind == TextureKind::D3 { 0 } else { view.layer() };
        let key = (res.generation, [view.level(), 1, slice, 1, format_key(format), kind as u32, u32::from_le_bytes([2,3,4,5])]);
        let mut views = self.views.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(texture) = views.get(&key) { return Ok(texture.clone()) }
        let texture = res.tex.view(&ViewDesc { format, kind, first_level: view.level(), level_count: 1,
            first_slice: slice, slice_count: 1 }).map_err(|e| crate::tex_skip(format_args!("attachment view {view:?} of {d:?}: {e}")))?;
        views.insert(key, texture.clone());
        Ok(texture)
    }

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
        let key = (rec.address.get(), desc);
        let r = if let Some(alias) = self.aliases.get(&key) {
            Res {
                fresh: false,
                ..alias.clone()
            }
        } else {
            if crate::diag("contain") {
                let (a0, a1) = (rec.address.get(), rec.address.get() + rec.layout.size);
                let mut spans = self.spans.lock().unwrap_or_else(PoisonError::into_inner);
                for &(b0, b1, other, od) in spans.iter() {
                    let inside = a0 >= b0 && a1 <= b1 && (a0, a1) != (b0, b1);
                    let around = b0 >= a0 && b1 <= a1 && (a0, a1) != (b0, b1);
                    if (inside || around) && other != id {
                        eprintln!(
                            "DIAG contain {} new {id:?} {:?} {}x{}x{} l{} {:?} [{a0:#x}+{:#x}] {} old {other:?} {:?} {}x{}x{} l{} {:?} [{b0:#x}+{:#x}] off {:#x}",
                            if inside { "inside" } else { "around" },
                            desc.kind, desc.width, desc.height, desc.array_layers, desc.mip_levels, desc.format, a1 - a0,
                            if inside { "of" } else { "holds" },
                            od.kind, od.width, od.height, od.array_layers, od.mip_levels, od.format, b1 - b0,
                            a0.abs_diff(b0)
                        );
                    }
                }
                if spans.len() < 20000 {
                    spans.push((a0, a1, id, desc));
                }
            }
            if crate::diag("split") {
                for ((a, other), r) in &self.aliases {
                    if *a == rec.address.get() {
                        eprintln!(
                            "DIAG split addr {a:#x} new {id:?} {:?} {}x{}x{} l{} {:?} rt {} | old {:?} {:?} {}x{}x{} l{} {:?} rt {}",
                            desc.kind, desc.width, desc.height, desc.array_layers, desc.mip_levels, desc.format, desc.render_target,
                            r.barrier_id, other.kind, other.width, other.height, other.array_layers, other.mip_levels, other.format, other.render_target
                        );
                    }
                }
            }
            if crate::diag("arr") && desc.array_layers == 8 && desc.width >= 256 {
                eprintln!(
                    "DIAG arr new metal texture {id:?} at {:#x} {:?} {}x{} gen {} (entry had {:?})",
                    rec.address.get(), desc.format, desc.width, desc.height, self.next_generation,
                    self.entries.get(&id).map(|r| r.generation)
                );
            }
            let tex = Texture::new(&self.ctx, desc)
                .map_err(|e| crate::tex_skip(format_args!("new {id:?} {desc:?}: {e:?}")))?;
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
            };
            self.aliases.insert(key, r.clone());
            stats.textures_created += 1;
            r
        };
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
    /// format and channel swizzle of the descriptor.
    pub(crate) fn view(
        &self,
        id: TexId,
        res: &Res,
        tic: &Tic,
        stats: &mut EncodeStats,
    ) -> Result<Texture, Skip> {
        self.view_as(id, res, tic, None, stats)
    }

    /// The view of [`TextureStore::view`] with the texture type `forced` instead of the one of the descriptor: the
    /// type that a shader declares for the binding. A 2D view shows the first layer of the range, a 2D array view the
    /// whole range, a cube view the first cube and a cube array view the whole cubes of the range.
    pub(crate) fn view_as(
        &self,
        id: TexId,
        res: &Res,
        tic: &Tic,
        forced: Option<TextureKind>,
        _stats: &mut EncodeStats,
    ) -> Result<Texture, Skip> {
        let d = res.desc;
        let levels = if tic.view.levels == 0 {
            d.mip_levels
        } else {
            u32::from(tic.view.levels)
        };
        let first_level = u32::from(tic.view.base_level).min(d.mip_levels - 1);
        let level_count = levels.min(d.mip_levels - first_level).max(1);
        let slices = d.slice_count();
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
        let wanted = forced.or_else(|| kind_of(tic.view.target.unwrap_or(tic.target)));
        let mut kind = match (d.kind, wanted) {
            (TextureKind::D3, _) => TextureKind::D3,
            (_, Some(TextureKind::D3)) | (_, None) => d.kind,
            (_, Some(k)) => k,
        };
        if matches!(kind, TextureKind::Cube | TextureKind::CubeArray) && slices < 6 {
            kind = TextureKind::D2Array;
        }
        if kind == TextureKind::Cube && crate::knob("cubearr") && slices >= 6 {
            kind = TextureKind::CubeArray;
        }
        // A cube view counts its layers in cubes, as the hardware's texture header does. `view_swizzled` takes the
        // first face of a cube and the first cube of a cube array.
        let base = u32::from(tic.view.base_layer);
        let count = u32::from(tic.view.layers);
        let (first_slice, slice_count) = match kind {
            TextureKind::D3 => (0, 1),
            TextureKind::D2 => (base.min(slices - 1), 1),
            TextureKind::D2Array => {
                let first = base.min(slices - 1);
                let n = if count == 0 { slices - first } else { count.min(slices - first) };
                (first, n.max(1))
            }
            TextureKind::Cube => {
                let cubes = slices / 6;
                (base.min(cubes - 1) * 6, 1)
            }
            TextureKind::CubeArray => {
                let cubes = slices / 6;
                let first = base.min(cubes - 1);
                let n = if count == 0 { cubes - first } else { count.min(cubes - first) };
                (first, n.max(1))
            }
        };
        let full = match kind {
            TextureKind::Cube => slice_count == 1 && slices == 6,
            TextureKind::CubeArray => slice_count * 6 == slices,
            _ => slice_count == slices,
        };
        let identity = first_level == 0
            && level_count == d.mip_levels
            && first_slice == 0
            && full
            && format == d.format
            && kind == d.kind
            && tic.view.swizzle == [2, 3, 4, 5];
        if identity || d.storage == Storage::Memoryless {
            return Ok(res.tex.clone());
        }
        let key: ViewKey = (
            res.generation,
            [
                first_level,
                level_count,
                first_slice,
                slice_count,
                format_key(format),
                kind as u32,
                u32::from_le_bytes(tic.view.swizzle),
            ],
        );
        let mut views = self.views.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(v) = views.get(&key) {
            return Ok(v.clone());
        }
        let v = res
            .tex
            .view_swizzled(&ViewDesc { format, kind, first_level, level_count, first_slice, slice_count }, tic.view.swizzle)
            .map_err(|e| {
                crate::tex_skip(format_args!(
                    "view {id:?} {:?} -> {format:?} {kind:?} l{first_level}+{level_count} s{first_slice}+{slice_count}: {e:?}",
                    d
                ))
            })?;
        views.insert(key, v.clone());
        Ok(v)
    }

    /// Drops the views of replaced textures.
    pub(crate) fn trim_views(&mut self) {
        let live: FastSet<u64> = self.entries.values().map(|v| v.generation).collect();
        self.aliases.retain(|_, r| live.contains(&r.generation));
        self.views
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(generation, _), _| live.contains(generation));
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

#[cfg(test)]
mod tests {
    use super::*;
    use nxkit_nvn::{resource::ResourceTable, space::GpuAddr};

    #[test]
    fn attachment_views_write_distinct_mips_and_layers() {
        use nxkit_metal::{ArgumentTable, ColorAttachment, FrameRing, LoadAction, RenderPassDesc, StoreAction};
        use nxkit_nvn::frame::TargetView;
        let Ok(ctx) = Context::system_default() else { return };
        let desc = nxkit_nvn::layout::TextureDesc { target: Target::D2Array, format: 0x25,
            flags: 0, samples: 0, width: 16, height: 16, depth: 2, levels: 2, stride: 0, packaged_block: None };
        let mut table = ResourceTable::new();
        let id = table.create(desc, GpuAddr::new(0x2000).unwrap()).unwrap();
        let mut store = TextureStore::new(&ctx);
        let res = store.ensure(table.get(id).unwrap(), 1.0, Use { attachment: true, ..Use::default() }, &mut EncodeStats::default()).unwrap();
        let first = store.target_view(&res, TargetView::from_words([3, 0, 1, 0])).unwrap();
        let second = store.target_view(&res, TargetView::from_words([0x00010003, 1 << 16, 1, 0])).unwrap();
        assert_eq!((first.desc().width, second.desc().width), (16, 8));
        let table = ArgumentTable::new(&ctx, 1, 1).unwrap();
        let mut ring = FrameRing::new(&ctx).unwrap();
        let mut frame = ring.begin().unwrap();
        for (texture, clear) in [(&first, [1.,0.,0.,1.]), (&second, [0.,1.,0.,1.])] {
            frame.commands().begin_render_pass(&RenderPassDesc { colors: vec![Some(ColorAttachment { depth_plane: 0,
                texture: texture.clone(), load: LoadAction::Clear, store: StoreAction::Store,
                clear, memoryless: false })], depth: None, stencil: None }, &table).unwrap().end();
        }
        frame.submit().unwrap().wait().unwrap();
        assert_eq!(first.debug_read(&ctx, &[[0,0]]).unwrap()[0], [255,0,0,255]);
        assert_eq!(second.debug_read(&ctx, &[[0,0]]).unwrap()[0], [0,255,0,255]);
        assert_eq!(store.target_view(&res, TargetView::from_words([3,0,1,0])).unwrap().gpu_resource_id(), first.gpu_resource_id());
    }

    #[test]
    fn repeated_objects_over_the_same_storage_share_one_backing() {
        let Ok(ctx) = Context::system_default() else {
            return;
        };
        let desc = nxkit_nvn::layout::TextureDesc {
            target: Target::D2,
            format: 0x25,
            flags: 0,
            samples: 0,
            width: 64,
            height: 64,
            depth: 1,
            levels: 1,
            stride: 0,
            packaged_block: None,
        };
        let mut table = ResourceTable::new();
        let mut store = TextureStore::new(&ctx);
        let mut stats = EncodeStats::default();
        let mut first = None;
        let mut ids = Vec::new();
        for _ in 0..1000 {
            let id = table.create(desc, GpuAddr::new(0x2000).unwrap()).unwrap();
            let res = store
                .ensure(table.get(id).unwrap(), 1.0, Use::default(), &mut stats)
                .unwrap();
            let key = (res.tex.gpu_resource_id(), res.generation, res.barrier_id);
            if let Some(want) = first {
                assert_eq!(key, want);
            } else {
                first = Some(key);
            }
            ids.push(id);
        }
        assert_eq!(stats.textures_created, 1);
        assert_eq!(store.aliases.len(), 1);
        let different = table
            .create(
                nxkit_nvn::layout::TextureDesc {
                    format: 0x27,
                    ..desc
                },
                GpuAddr::new(0x2000).unwrap(),
            )
            .unwrap();
        let r = store
            .ensure(
                table.get(different).unwrap(),
                1.0,
                Use::default(),
                &mut stats,
            )
            .unwrap();
        assert_ne!(r.tex.gpu_resource_id(), first.unwrap().0);
        assert_eq!(stats.textures_created, 2);
        for id in ids {
            store.invalidate(id);
        }
        store.invalidate(different);
        store.trim_views();
        assert!(store.aliases.is_empty());
    }
}
