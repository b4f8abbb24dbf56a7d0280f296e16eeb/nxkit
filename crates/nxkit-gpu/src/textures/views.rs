//! The views of the texture store: attachment views, sampling views, image views and the view cache trim.

use super::*;

impl TextureStore {
    /// A single renderable mip/layer. Sampling views and attachment views share the cache.
    pub(crate) fn target_view(
        &self,
        res: &Res,
        view: nxkit_nvn::frame::TargetView,
    ) -> Result<Texture, Skip> {
        let d = res.desc;
        // Metal aborts on a view between compressed and uncompressed formats, and cannot render into a compressed one:
        // the encoder renders such an attachment into a block shadow (`TextureStore::block_target`).
        if d.format.is_compressed() {
            let what = format_args!("attachment view {view:?} of compressed {d:?}");
            nxkit_nvn::diag::note(nxkit_nvn::diag::Reason::ViewCreate, what);
            return Err(crate::tex_skip(what));
        }
        let format = match view.format().and_then(texture_format) {
            Some(Mapped::Exact(format)) if !res.widened => format,
            _ => d.format,
        };
        if d.kind == TextureKind::D2
            && d.mip_levels == 1
            && view.level() == 0
            && view.layer() == 0
            && format == d.format
        {
            return Ok(res.tex.clone());
        }
        let kind = if d.kind == TextureKind::D3 {
            TextureKind::D3
        } else {
            TextureKind::D2
        };
        let slice = if kind == TextureKind::D3 {
            0
        } else {
            view.layer()
        };
        let key = (
            res.generation,
            [
                view.level(),
                1,
                slice,
                1,
                format_key(format),
                kind as u32,
                u32::from_le_bytes([2, 3, 4, 5]),
            ],
        );
        let mut views = self.views.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(texture) = views.get(&key) {
            return Ok(texture.clone());
        }
        let texture = res
            .tex
            .view(&ViewDesc {
                format,
                kind,
                first_level: view.level(),
                level_count: 1,
                first_slice: slice,
                slice_count: 1,
            })
            .map_err(|e| {
                let what = format_args!("attachment view {view:?} of {d:?}: {e}");
                nxkit_nvn::diag::note(nxkit_nvn::diag::Reason::ViewCreate, what);
                crate::tex_skip(what)
            })?;
        views.insert(key, texture.clone());
        Ok(texture)
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
        stats: &mut EncodeStats,
    ) -> Result<Texture, Skip> {
        self.view_impl(id, res, tic, forced, 0, stats)
    }

    /// The view a shader writes or reads as a storage image: one level (the base level of the descriptor), the layers of
    /// the descriptor, no channel swizzle, in the texture type `kind` that the shader declares. `atomic` is
    /// `ImgDecl::atomic_code`: 0 for a plain image, 1 or 2 for a view on which the shader runs atomics on unsigned or
    /// signed texels (`R32Uint` or `R32Sint`, over the same bytes).
    pub(crate) fn image_view(
        &self,
        id: TexId,
        res: &Res,
        tic: &Tic,
        kind: TextureKind,
        atomic: u8,
        stats: &mut EncodeStats,
    ) -> Result<Texture, Skip> {
        let mut t = *tic;
        t.view.levels = 1;
        t.view.swizzle = [2, 3, 4, 5];
        self.view_impl(id, res, &t, Some(kind), atomic, stats)
    }

    fn view_impl(
        &self,
        id: TexId,
        res: &Res,
        tic: &Tic,
        forced: Option<TextureKind>,
        atomic: u8,
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
                let n = if count == 0 {
                    slices - first
                } else {
                    count.min(slices - first)
                };
                (first, n.max(1))
            }
            TextureKind::Cube => {
                let cubes = slices / 6;
                (base.min(cubes - 1) * 6, 1)
            }
            TextureKind::CubeArray => {
                let cubes = slices / 6;
                let first = base.min(cubes - 1);
                let n = if count == 0 {
                    cubes - first
                } else {
                    count.min(cubes - first)
                };
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
        if (identity && atomic == 0) || d.storage == Storage::Memoryless {
            return Ok(res.tex.clone());
        }
        let key: ViewKey = (
            res.generation,
            [
                first_level,
                level_count,
                first_slice,
                slice_count,
                format_key(format) ^ (u32::from(atomic) << 30),
                kind as u32,
                u32::from_le_bytes(tic.view.swizzle),
            ],
        );
        let mut views = self.views.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(v) = views.get(&key) {
            return Ok(v.clone());
        }
        let vd = ViewDesc {
            format,
            kind,
            first_level,
            level_count,
            first_slice,
            slice_count,
        };
        let made = if atomic == 0 {
            res.tex.view_swizzled(&vd, tic.view.swizzle)
        } else {
            res.tex.atomic_view(&vd, atomic == 2)
        };
        let v = made
            .map_err(|e| {
                let what = format_args!(
                    "view {id:?} {:?} -> {format:?} {kind:?} l{first_level}+{level_count} s{first_slice}+{slice_count}: {e:?}",
                    d
                );
                nxkit_nvn::diag::note(nxkit_nvn::diag::Reason::ViewCreate, what);
                crate::tex_skip(what)
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
        self.shadows
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(generation, ..), _| live.contains(generation));
    }
}
