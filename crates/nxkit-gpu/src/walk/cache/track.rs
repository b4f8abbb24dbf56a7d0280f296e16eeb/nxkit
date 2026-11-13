//! What a walk tracks about a recording while it runs, and what a template keeps of it.

use super::*;

/// The source of one handle of a `BindTexturesDeferred`, and the handle that it gave.
#[derive(Clone, Copy, Debug)]
pub(in crate::walk) struct DepSrc {
    pub(super) src: u64,
    pub(super) k: u8,
    pub(super) tex_base: u32,
    pub(super) samp_base: u32,
    pub(super) handle: u64,
}

/// A texture of a draw or a dispatch that came from a deferred source.
#[derive(Clone, Copy, Debug)]
pub(super) struct Patch {
    /// The ordinal of the draw or the dispatch.
    pub(super) idx: u32,
    pub(super) dispatch: bool,
    /// The position in the textures of the entry.
    pub(super) pos: u16,
    /// The index in the deferred sources.
    pub(super) dep: u32,
    /// The texture slot of the bank layout of the walker (0 to 127).
    pub(super) slot: u8,
}

/// An inline uniform update, with the draws and dispatches that came before it.
#[derive(Clone, Copy, Debug)]
pub(in crate::walk) struct Upd {
    pub(super) ext: Ext,
    pub(super) draws: u32,
    pub(super) dispatches: u32,
}

/// What a recording left in one field of the state.
#[derive(Clone, Copy, Debug)]
pub(super) enum Delta {
    Program(u8, u64),
    VertexBuffer(u8, (u64, u64)),
    Ubo(u8, (u64, u32)),
    Sbo(u8, (u64, u32)),
    Attribute(u8, Option<(u32, u32, u32)>),
    Stream(u8, (u32, u32)),
    Blend(u8, u64),
    Texture(u8, u64),
    /// A texture from the deferred source with this index.
    TextureDeferred(u8, u32),
}

/// The scalar fields that a recording set, with their values at its end.
#[derive(Clone, Debug)]
pub(super) struct Scalars {
    pub(super) groups: u32,
    enable: u8,
    masks: [u8; 8],
    depth_stencil: [u32; 2],
    stencil: StencilValues,
    raster: Raster,
    viewport: [f32; 4],
    viewport_swizzle: [u32; 4],
    origins: [bool; 2],
    depth_range: [f32; 2],
    scissor: Option<[u32; 4]>,
    blend_color: [f32; 4],
    copy_strides: [u64; 2],
}

/// The bits of floats, so that a key compares them as a walk sees them: -0.0 differs from 0.0.
pub(super) fn bits2(v: [f32; 2]) -> [u32; 2] {
    v.map(f32::to_bits)
}

pub(super) fn bits3(v: [f32; 3]) -> [u32; 3] {
    v.map(f32::to_bits)
}

pub(super) fn bits4(v: [f32; 4]) -> [u32; 4] {
    v.map(f32::to_bits)
}

impl Scalars {
    pub(super) fn of(st: &State, groups: u32) -> Self {
        Self {
            groups,
            enable: st.blends.enable(),
            masks: st.masks,
            depth_stencil: st.depth_stencil,
            stencil: st.stencil,
            raster: st.raster,
            viewport: st.viewport,
            viewport_swizzle: st.viewport_swizzle,
            origins: st.origins,
            depth_range: st.depth_range,
            scissor: st.scissor,
            blend_color: st.blend_color,
            copy_strides: st.copy_strides,
        }
    }

    /// Whether `st` has the values of the groups that this snapshot names.
    pub(super) fn matches(&self, st: &State) -> bool {
        let has = |g: usize| self.groups >> g & 1 != 0;
        let (r, o) = (&st.raster, &self.raster);
        (!has(sc::BLEND_ENABLE) || st.blends.enable() == self.enable)
            && (!has(sc::MASKS) || st.masks == self.masks)
            && (!has(sc::DEPTH_STENCIL) || st.depth_stencil == self.depth_stencil)
            && (0..2).all(|f| {
                (!has(sc::STENCIL_READ + f) || st.stencil.read[f] == self.stencil.read[f])
                    && (!has(sc::STENCIL_WRITE + f) || st.stencil.write[f] == self.stencil.write[f])
                    && (!has(sc::STENCIL_REF + f)
                        || st.stencil.reference[f] == self.stencil.reference[f])
            })
            && (!has(sc::POLYGON)
                || (r.cull, r.front_face, r.polygon_mode, r.offset_on)
                    == (o.cull, o.front_face, o.polygon_mode, o.offset_on))
            && (!has(sc::MULTISAMPLE) || r.alpha_to_coverage == o.alpha_to_coverage)
            && (!has(sc::OFFSET) || bits3(r.offset) == bits3(o.offset))
            && (!has(sc::CLAMP) || r.clamp == o.clamp)
            && (!has(sc::VIEWPORT) || bits4(st.viewport) == bits4(self.viewport))
            && (!has(sc::VIEWPORT_SWIZZLE) || st.viewport_swizzle == self.viewport_swizzle)
            && (!has(sc::ORIGINS) || st.origins == self.origins)
            && (!has(sc::DEPTH_RANGE) || bits2(st.depth_range) == bits2(self.depth_range))
            && (!has(sc::SCISSOR) || st.scissor == self.scissor)
            && (!has(sc::BLEND_COLOR) || bits4(st.blend_color) == bits4(self.blend_color))
            && (!has(sc::STRIDE_ROW) || st.copy_strides[0] == self.copy_strides[0])
            && (!has(sc::STRIDE_IMAGE) || st.copy_strides[1] == self.copy_strides[1])
    }

    pub(super) fn apply(&self, st: &mut State) {
        let has = |g: usize| self.groups >> g & 1 != 0;
        if has(sc::BLEND_ENABLE) {
            st.blends.set_enable(self.enable);
        }
        if has(sc::MASKS) {
            st.masks = self.masks;
        }
        if has(sc::DEPTH_STENCIL) {
            st.depth_stencil = self.depth_stencil;
        }
        for face in 0..2 {
            if has(sc::STENCIL_READ + face) {
                st.stencil.read[face] = self.stencil.read[face];
            }
            if has(sc::STENCIL_WRITE + face) {
                st.stencil.write[face] = self.stencil.write[face];
            }
            if has(sc::STENCIL_REF + face) {
                st.stencil.reference[face] = self.stencil.reference[face];
            }
        }
        if has(sc::POLYGON) {
            let (r, s) = (&mut st.raster, &self.raster);
            (r.cull, r.front_face, r.polygon_mode, r.offset_on) =
                (s.cull, s.front_face, s.polygon_mode, s.offset_on);
        }
        if has(sc::MULTISAMPLE) {
            st.raster.alpha_to_coverage = self.raster.alpha_to_coverage;
        }
        if has(sc::OFFSET) {
            st.raster.offset = self.raster.offset;
        }
        if has(sc::CLAMP) {
            st.raster.clamp = self.raster.clamp;
        }
        if has(sc::VIEWPORT) {
            st.viewport = self.viewport;
        }
        if has(sc::VIEWPORT_SWIZZLE) {
            st.viewport_swizzle = self.viewport_swizzle;
        }
        if has(sc::ORIGINS) {
            st.origins = self.origins;
        }
        if has(sc::DEPTH_RANGE) {
            st.depth_range = self.depth_range;
        }
        if has(sc::SCISSOR) {
            st.scissor = self.scissor;
        }
        if has(sc::BLEND_COLOR) {
            st.blend_color = self.blend_color;
        }
        if has(sc::STRIDE_ROW) {
            st.copy_strides[0] = self.copy_strides[0];
        }
        if has(sc::STRIDE_IMAGE) {
            st.copy_strides[1] = self.copy_strides[1];
        }
    }
}

/// What the walk of one recording produced, for one state at its entry.
pub(in crate::walk) struct Template {
    pub(super) needs: Bits,
    pub(super) written: Bits,
    pub(super) key: Key,
    /// The deepest nesting of calls inside the recording. The walk cuts calls at [`MAX_CALL_DEPTH`].
    pub(super) height: u32,
    /// The program epoch at which the generations were last found current.
    pub(super) epoch: u64,
    pub(super) programs: Vec<(u64, u64)>,
    pub(super) spans: Vec<(u64, u64)>,
    pub(super) draws: Vec<Arc<DrawDetail>>,
    pub(super) dispatches: Vec<DispatchDetail>,
    pub(super) clears: Vec<ClearDetail>,
    pub(super) copies: Vec<CopyDetail>,
    pub(super) reports: Vec<CounterReport>,
    pub(super) updates: Vec<Upd>,
    pub(super) deferred: Vec<DepSrc>,
    pub(super) patches: Vec<Patch>,
    /// The textures whose slot the recording inherited from a deferred source of its caller. A hit gives them to the
    /// caller's patches, because the caller's template must follow that source.
    pub(super) inherited: Vec<Patch>,
    pub(super) delta: Vec<Delta>,
    pub(super) scalars: Option<Box<Scalars>>,
    /// The distinct uniform buffer ranges of the draws and dispatches, with the snapshot that they had. Only for
    /// a template without updates, where the snapshot of a range is the same for all its draws.
    pub(super) ranges: RangeSnapshots,
}

/// How many entries each output vector of the frame detail had.
#[derive(Clone, Copy, Debug, Default)]
pub(in crate::walk) struct Marks {
    pub(super) draws: usize,
    pub(super) clears: usize,
    pub(super) copies: usize,
    pub(super) dispatches: usize,
    pub(super) reports: usize,
}

/// A recording that is being walked for its template.
pub(in crate::walk) struct Open {
    pub(super) needs: Bits,
    pub(super) written: Bits,
    pub(super) entry: Box<State>,
    /// `Track::prov` at the entry.
    pub(super) prov: u128,
    pub(super) marks: Marks,
    pub(super) dep0: usize,
    pub(super) patch0: usize,
    pub(super) upd0: usize,
    pub(super) programs: Vec<u64>,
    pub(super) spans: Vec<(u64, u64)>,
    pub(super) height: u32,
    pub(super) truncated: bool,
}

impl Open {
    pub(super) fn absorb(
        &mut self,
        needs: &Bits,
        written: &Bits,
        programs: impl Iterator<Item = u64>,
        spans: &[(u64, u64)],
        height: u32,
        truncated: bool,
    ) {
        self.needs.add_missing(needs, &self.written);
        self.written.add(written);
        for p in programs {
            if !self.programs.contains(&p) {
                self.programs.push(p);
            }
        }
        self.spans.extend_from_slice(spans);
        self.height = self.height.max(height + 1);
        self.truncated |= truncated;
    }
}

/// The logs of the walk of one top-level recording, and the recordings that are open in it.
pub(in crate::walk) struct Track {
    pub(super) open: Vec<Open>,
    /// Entry states of finished recordings, kept so that a walk does not allocate one for each recording.
    #[allow(clippy::vec_box)]
    pub(super) pool: Vec<Box<State>>,
    pub(super) dep_list: Vec<DepSrc>,
    pub(super) patches: Vec<Patch>,
    pub(super) upd_log: Vec<Upd>,
    /// For each texture slot, the index in `dep_list` of the source that set it, or [`NO_DEP`].
    pub(super) tex_dep: [u32; TEXTURE_SLOTS],
    /// Bit n is set when `tex_dep` of slot n is not [`NO_DEP`]. A template is only valid for the same provenance of the
    /// slots it reads, because only a slot from a deferred source needs a patch.
    pub(super) prov: u128,
}

impl Track {
    pub(in crate::walk) fn new() -> Self {
        Self {
            open: Vec::new(),
            pool: Vec::new(),
            dep_list: Vec::new(),
            patches: Vec::new(),
            upd_log: Vec::new(),
            tex_dep: [NO_DEP; TEXTURE_SLOTS],
            prov: 0,
        }
    }

    pub(super) fn reset(&mut self) {
        while let Some(f) = self.open.pop() {
            self.pool.push(f.entry);
        }
        self.dep_list.clear();
        self.patches.clear();
        self.upd_log.clear();
        self.tex_dep = [NO_DEP; TEXTURE_SLOTS];
        self.prov = 0;
    }

    /// Whether a recording is being walked for a template.
    pub(in crate::walk) fn tracking(&self) -> bool {
        !self.open.is_empty()
    }

    /// The walk reads the fields of `m`.
    pub(in crate::walk) fn read(&mut self, m: &Bits) {
        if let Some(f) = self.open.last_mut() {
            f.needs.add_missing(m, &f.written);
        }
    }

    /// The walk reads one field.
    pub(in crate::walk) fn read_bit(&mut self, bit: usize) {
        if let Some(f) = self.open.last_mut()
            && f.written.0[bit / 64] >> (bit % 64) & 1 == 0
        {
            f.needs.set(bit);
        }
    }

    /// The walk sets one field.
    pub(in crate::walk) fn write(&mut self, bit: usize) {
        if let Some(f) = self.open.last_mut() {
            f.written.set(bit);
        }
    }

    /// The walk looks up `program`, which is in the slot `stage`.
    pub(in crate::walk) fn program(&mut self, stage: usize, program: u64) {
        self.read_bit(program_bit(stage));
        if let Some(f) = self.open.last_mut()
            && !f.programs.contains(&program)
        {
            f.programs.push(program);
        }
    }

    /// The walk ended a run for a limit, so the recording is cut and cannot be a template.
    pub(in crate::walk) fn truncated(&mut self) {
        if let Some(f) = self.open.last_mut() {
            f.truncated = true;
        }
    }

    /// The walk read the bytes `[start, end)` of a recording.
    pub(in crate::walk) fn span(&mut self, start: u64, end: u64) {
        if let Some(f) = self.open.last_mut() {
            match f.spans.last_mut() {
                Some((_, e)) if *e == start => *e = end,
                _ => f.spans.push((start, end)),
            }
        }
    }

    /// A texture slot got a constant handle.
    pub(in crate::walk) fn texture_set(&mut self, slot: usize) {
        self.write(texture_bit(slot));
        if let Some(d) = self.tex_dep.get_mut(slot) {
            *d = NO_DEP;
            self.prov &= !(1u128 << slot);
        }
    }

    /// A texture slot got the handle of a deferred source.
    pub(in crate::walk) fn texture_deferred(&mut self, slot: usize, src: DepSrc) {
        let at = self.dep_list.len() as u32;
        self.dep_list.push(src);
        self.write(texture_bit(slot));
        if let Some(d) = self.tex_dep.get_mut(slot) {
            *d = at;
            self.prov |= 1u128 << slot;
        }
    }

    /// The deferred source for the handle `k` of a bind.
    pub(in crate::walk) fn source(
        src: u64,
        k: u8,
        tex_base: u32,
        samp_base: u32,
        handle: u64,
    ) -> DepSrc {
        DepSrc {
            src,
            k,
            tex_base,
            samp_base,
            handle,
        }
    }

    /// Logs an inline update at the current ends of the draws and dispatches.
    pub(in crate::walk) fn update(&mut self, ext: &Ext, draws: usize, dispatches: usize) {
        if self.tracking() {
            self.upd_log.push(Upd {
                ext: *ext,
                draws: draws as u32,
                dispatches: dispatches as u32,
            });
        }
    }

    /// Logs the textures of a draw or a dispatch that came from deferred sources.
    pub(in crate::walk) fn patches_of(
        &mut self,
        textures: &[TextureBind],
        idx: usize,
        dispatch: bool,
    ) {
        if self.open.is_empty() || self.dep_list.is_empty() {
            return;
        }
        for (pos, t) in textures.iter().enumerate() {
            let bank = match t.stage {
                Stage::Vertex => 0,
                Stage::Fragment => 1,
                Stage::Compute => 2,
                Stage::Task | Stage::Mesh => 3,
            };
            let dep = self
                .tex_dep
                .get(bank * 32 + t.slot as usize)
                .copied()
                .unwrap_or(NO_DEP);
            if dep != NO_DEP {
                let slot = (bank * 32 + t.slot as usize) as u8;
                self.patches.push(Patch {
                    idx: idx as u32,
                    dispatch,
                    pos: pos as u16,
                    dep,
                    slot,
                });
            }
        }
    }
}
