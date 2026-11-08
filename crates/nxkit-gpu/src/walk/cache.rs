//! The template cache of the walker.
//!
//! A game replays the recordings of its models in every frame. A walk reads each record again, follows the binds and
//! builds the same [`DrawDetail`] values. The cache keeps what the walk of a recording produced, and a later call of the
//! same handle plays that back.
//!
//! A [`Template`] is valid for the state that the recording inherits and reads. The walk tracks, for each recording, which
//! fields of the [`State`] it reads before it sets them (`needs`). The `key` holds their values at the entry. A call
//! matches a template when its state has the same values in those fields. A handle keeps up to [`MAX_PER_HANDLE`]
//! templates for different states.
//!
//! What a template holds, and how a hit keeps it exact:
//!
//! - The draws, clears, copies, dispatches and reports that the walk pushed, in the order of the walk, so the ordinals
//!   agree with the decoder. A draw is shared with the frame detail through an `Arc`.
//! - The state that the recording leaves behind (`delta`): the fields it set, with the values they have at its end.
//! - The sources of `BindTexturesDeferred`. A hit reads their values again. A draw whose texture came from a source
//!   that changed gets the new handle (`patches`), and the template keeps the new values.
//! - The inline `UpdateUniform` records. A hit writes their bytes into guest memory again, in order, as the walk does.
//! - The snapshots of uniform buffers. A snapshot depends on the memory that the game may rewrite without a record, so a
//!   hit looks at the current bytes and rebuilds a draw only when they differ.
//! - The generations of the programs that it looked up. `program_set` and `program_released` make it stale.
//!
//! A template is dropped when the game writes the memory of one of its records (`Walker::invalidate`). The spans of a
//! template include those of the recordings it calls, so a write to a callee drops its callers too.
//!
//! The recordings that [`Walker::submit`] is given are not cached. The game writes them anew in every frame, and the
//! recordings they call are the ones that repeat.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::sync::atomic::{AtomicBool, Ordering};

use nxkit_nvn::ext::Ext;
use nxkit_nvn::mem::GuestMem;
use nxkit_nvn::record::deferred_handle;

use super::objects::{Raster, StencilValues};
use super::{MAX_CALL_DEPTH, STREAMS, State, TEXTURE_SLOTS, Walker};
use crate::detail::{
    ClearDetail, CopyDetail, CounterReport, DispatchDetail, DrawDetail, Stage, TextureBind,
};
use crate::draws::FastMap;

/// The most templates that one handle keeps. The least recently used goes first.
///
/// A model recording runs once per instance and pass, with another program, uniform buffer or vertex buffer each
/// time; BotW calls one recording with more than four states in a frame. The key digest keeps the search cheap.
pub(super) const MAX_PER_HANDLE: usize = 32;
/// The most handles with templates. The cache starts again when a new handle would pass it.
const MAX_HANDLES: usize = 1 << 16;
/// The value of `Track::tex_dep` for a slot that no deferred bind set.
const NO_DEP: u32 = u32::MAX;

const WORDS: usize = 7;

/// A set of fields of the walker state. One bit is one slot or one group of values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Bits([u64; WORDS]);

impl Bits {
    const NONE: Bits = Bits([0; WORDS]);

    const fn range(start: usize, len: usize) -> Bits {
        let mut w = [0u64; WORDS];
        let mut i = start;
        while i < start + len {
            w[i / 64] |= 1 << (i % 64);
            i += 1;
        }
        Bits(w)
    }

    const fn or(self, o: Bits) -> Bits {
        let mut w = self.0;
        let mut i = 0;
        while i < WORDS {
            w[i] |= o.0[i];
            i += 1;
        }
        Bits(w)
    }

    /// The scalar groups of `list` (indices of [`sc`]).
    const fn scalars(list: &[usize]) -> Bits {
        let mut b = Bits::NONE;
        let mut i = 0;
        while i < list.len() {
            b = b.or(Bits::range(SC + list[i], 1));
            i += 1;
        }
        b
    }

    fn has(&self, bit: usize) -> bool {
        self.0.get(bit / 64).is_some_and(|w| w >> (bit % 64) & 1 != 0)
    }

    fn set(&mut self, bit: usize) {
        if let Some(w) = self.0.get_mut(bit / 64) {
            *w |= 1 << (bit % 64);
        }
    }

    /// Adds the bits of `m` that `except` does not have.
    fn add_missing(&mut self, m: &Bits, except: &Bits) {
        for ((w, m), e) in self.0.iter_mut().zip(&m.0).zip(&except.0) {
            *w |= m & !e;
        }
    }

    fn add(&mut self, m: &Bits) {
        for (w, m) in self.0.iter_mut().zip(&m.0) {
            *w |= m;
        }
    }

    fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.0.iter().enumerate().flat_map(|(w, bits)| {
            let mut rest = *bits;
            std::iter::from_fn(move || {
                if rest == 0 {
                    return None;
                }
                let at = rest.trailing_zeros() as usize;
                rest &= rest - 1;
                Some(w * 64 + at)
            })
        })
    }
}

/// The first bit of each kind of field.
const P: usize = 0;
const VB: usize = P + 6;
const UBO: usize = VB + STREAMS;
const SBO: usize = UBO + 16 * super::BANKS;
const ATTR: usize = SBO + 16 * super::BANKS;
const STREAM: usize = ATTR + super::ATTRIBUTES;
const BLEND: usize = STREAM + STREAMS;
const TEX: usize = BLEND + 8;
const SC: usize = TEX + TEXTURE_SLOTS;

/// The groups of scalar fields, as an index from [`SC`].
pub(super) mod sc {
    pub(in crate::walk) const BLEND_ENABLE: usize = 0;
    pub(in crate::walk) const MASKS: usize = 1;
    pub(in crate::walk) const DEPTH_STENCIL: usize = 2;
    pub(in crate::walk) const STENCIL_READ: usize = 3;
    pub(in crate::walk) const STENCIL_WRITE: usize = 5;
    pub(in crate::walk) const STENCIL_REF: usize = 7;
    pub(in crate::walk) const POLYGON: usize = 9;
    pub(in crate::walk) const MULTISAMPLE: usize = 10;
    pub(in crate::walk) const OFFSET: usize = 11;
    pub(in crate::walk) const CLAMP: usize = 12;
    pub(in crate::walk) const VIEWPORT: usize = 13;
    pub(in crate::walk) const DEPTH_RANGE: usize = 14;
    pub(in crate::walk) const SCISSOR: usize = 15;
    pub(in crate::walk) const BLEND_COLOR: usize = 16;
    pub(in crate::walk) const STRIDE_ROW: usize = 17;
    pub(in crate::walk) const STRIDE_IMAGE: usize = 18;
}

/// The bit of the program slot `stage`.
pub(super) const fn program_bit(stage: usize) -> usize {
    P + stage
}

/// The bit of the vertex buffer `stream`.
pub(super) const fn vertex_buffer_bit(stream: usize) -> usize {
    VB + stream
}

/// The bit of the vertex stream `stream` (stride and divisor).
pub(super) const fn stream_bit(stream: usize) -> usize {
    STREAM + stream
}

/// The bit of the vertex attribute `location`.
pub(super) const fn attribute_bit(location: usize) -> usize {
    ATTR + location
}

/// The bit of the uniform buffer `slot` of the bank (0 vertex, 1 fragment, 2 compute, 3 task, 4 mesh).
pub(super) const fn ubo_bit(bank: usize, slot: usize) -> usize {
    UBO + bank * 16 + slot
}

/// The bit of the storage buffer `slot` of the bank.
pub(super) const fn sbo_bit(bank: usize, slot: usize) -> usize {
    SBO + bank * 16 + slot
}

/// The bit of the blend object of `target`.
pub(super) const fn blend_bit(target: usize) -> usize {
    BLEND + target
}

/// The bit of the texture handle in `slot`.
pub(super) const fn texture_bit(slot: usize) -> usize {
    TEX + slot
}

/// The bit of a scalar group.
pub(super) const fn scalar_bit(group: usize) -> usize {
    SC + group
}

/// What a draw reads of the state, apart from the programs and the vertex buffers of its streams.
pub(super) const DRAW_READS: Bits = Bits::range(BLEND, 8)
    .or(Bits::range(ATTR, super::ATTRIBUTES))
    .or(Bits::range(UBO, 32))
    .or(Bits::range(SBO, 32))
    .or(Bits::range(TEX, 64))
    .or(Bits::scalars(&[
        sc::BLEND_ENABLE,
        sc::MASKS,
        sc::DEPTH_STENCIL,
        sc::STENCIL_READ,
        sc::STENCIL_READ + 1,
        sc::STENCIL_WRITE,
        sc::STENCIL_WRITE + 1,
        sc::STENCIL_REF,
        sc::STENCIL_REF + 1,
        sc::POLYGON,
        sc::MULTISAMPLE,
        sc::OFFSET,
        sc::CLAMP,
        sc::VIEWPORT,
        sc::DEPTH_RANGE,
        sc::SCISSOR,
        sc::BLEND_COLOR,
    ]));

/// What a mesh draw reads besides [`DRAW_READS`]: the buffers of the task and mesh banks and the texture bank that the
/// two stages share.
pub(super) const MESH_READS: Bits = Bits::range(UBO + 48, 32)
    .or(Bits::range(SBO + 48, 32))
    .or(Bits::range(TEX + 96, 32));

/// What a dispatch reads of the state, apart from its program.
pub(super) const DISPATCH_READS: Bits = Bits::range(UBO + 32, 16)
    .or(Bits::range(SBO + 32, 16))
    .or(Bits::range(TEX + 64, 32));

/// What a copy reads of the state.
pub(super) const COPY_READS: Bits = Bits::scalars(&[sc::STRIDE_ROW, sc::STRIDE_IMAGE]);

/// The source of one handle of a `BindTexturesDeferred`, and the handle that it gave.
#[derive(Clone, Copy, Debug)]
pub(super) struct DepSrc {
    src: u64,
    k: u8,
    tex_base: u32,
    samp_base: u32,
    handle: u64,
}

/// A texture of a draw or a dispatch that came from a deferred source.
#[derive(Clone, Copy, Debug)]
struct Patch {
    /// The ordinal of the draw or the dispatch.
    idx: u32,
    dispatch: bool,
    /// The position in the textures of the entry.
    pos: u16,
    /// The index in the deferred sources.
    dep: u32,
    /// The texture slot of the bank layout of the walker (0 to 127).
    slot: u8,
}

/// An inline uniform update, with the draws and dispatches that came before it.
#[derive(Clone, Copy, Debug)]
pub(super) struct Upd {
    ext: Ext,
    draws: u32,
    dispatches: u32,
}

/// What a recording left in one field of the state.
#[derive(Clone, Copy, Debug)]
enum Delta {
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
struct Scalars {
    groups: u32,
    enable: u8,
    masks: [u8; 8],
    depth_stencil: [u32; 2],
    stencil: StencilValues,
    raster: Raster,
    viewport: [f32; 4],
    depth_range: [f32; 2],
    scissor: Option<[u32; 4]>,
    blend_color: [f32; 4],
    copy_strides: [u64; 2],
}

/// The bits of floats, so that a key compares them as a walk sees them: -0.0 differs from 0.0.
fn bits2(v: [f32; 2]) -> [u32; 2] {
    v.map(f32::to_bits)
}

fn bits3(v: [f32; 3]) -> [u32; 3] {
    v.map(f32::to_bits)
}

fn bits4(v: [f32; 4]) -> [u32; 4] {
    v.map(f32::to_bits)
}

impl Scalars {
    fn of(st: &State, groups: u32) -> Self {
        Self {
            groups,
            enable: st.blends.enable(),
            masks: st.masks,
            depth_stencil: st.depth_stencil,
            stencil: st.stencil,
            raster: st.raster,
            viewport: st.viewport,
            depth_range: st.depth_range,
            scissor: st.scissor,
            blend_color: st.blend_color,
            copy_strides: st.copy_strides,
        }
    }

    /// Whether `st` has the values of the groups that this snapshot names.
    fn matches(&self, st: &State) -> bool {
        let has = |g: usize| self.groups >> g & 1 != 0;
        let (r, o) = (&st.raster, &self.raster);
        (!has(sc::BLEND_ENABLE) || st.blends.enable() == self.enable)
            && (!has(sc::MASKS) || st.masks == self.masks)
            && (!has(sc::DEPTH_STENCIL) || st.depth_stencil == self.depth_stencil)
            && (0..2).all(|f| {
                (!has(sc::STENCIL_READ + f) || st.stencil.read[f] == self.stencil.read[f])
                    && (!has(sc::STENCIL_WRITE + f) || st.stencil.write[f] == self.stencil.write[f])
                    && (!has(sc::STENCIL_REF + f) || st.stencil.reference[f] == self.stencil.reference[f])
            })
            && (!has(sc::POLYGON)
                || (r.cull, r.front_face, r.polygon_mode, r.offset_on) == (o.cull, o.front_face, o.polygon_mode, o.offset_on))
            && (!has(sc::MULTISAMPLE) || r.alpha_to_coverage == o.alpha_to_coverage)
            && (!has(sc::OFFSET) || bits3(r.offset) == bits3(o.offset))
            && (!has(sc::CLAMP) || r.clamp == o.clamp)
            && (!has(sc::VIEWPORT) || bits4(st.viewport) == bits4(self.viewport))
            && (!has(sc::DEPTH_RANGE) || bits2(st.depth_range) == bits2(self.depth_range))
            && (!has(sc::SCISSOR) || st.scissor == self.scissor)
            && (!has(sc::BLEND_COLOR) || bits4(st.blend_color) == bits4(self.blend_color))
            && (!has(sc::STRIDE_ROW) || st.copy_strides[0] == self.copy_strides[0])
            && (!has(sc::STRIDE_IMAGE) || st.copy_strides[1] == self.copy_strides[1])
    }

    fn apply(&self, st: &mut State) {
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
            (r.cull, r.front_face, r.polygon_mode, r.offset_on) = (s.cull, s.front_face, s.polygon_mode, s.offset_on);
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
pub(super) struct Template {
    needs: Bits,
    written: Bits,
    key: Key,
    /// The deepest nesting of calls inside the recording. The walk cuts calls at [`MAX_CALL_DEPTH`].
    height: u32,
    /// The program epoch at which the generations were last found current.
    epoch: u64,
    programs: Vec<(u64, u64)>,
    spans: Vec<(u64, u64)>,
    draws: Vec<Arc<DrawDetail>>,
    dispatches: Vec<DispatchDetail>,
    clears: Vec<ClearDetail>,
    copies: Vec<CopyDetail>,
    reports: Vec<CounterReport>,
    updates: Vec<Upd>,
    deferred: Vec<DepSrc>,
    patches: Vec<Patch>,
    /// The textures whose slot the recording inherited from a deferred source of its caller. A hit gives them to the
    /// caller's patches, because the caller's template must follow that source.
    inherited: Vec<Patch>,
    delta: Vec<Delta>,
    scalars: Option<Box<Scalars>>,
    /// The distinct uniform buffer ranges of the draws and dispatches, with the snapshot that they had. Only for
    /// a template without updates, where the snapshot of a range is the same for all its draws.
    ranges: RangeSnapshots,
}

/// How many entries each output vector of the frame detail had.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Marks {
    draws: usize,
    clears: usize,
    copies: usize,
    dispatches: usize,
    reports: usize,
}

/// A recording that is being walked for its template.
pub(super) struct Open {
    needs: Bits,
    written: Bits,
    entry: Box<State>,
    /// `Track::prov` at the entry.
    prov: u128,
    marks: Marks,
    dep0: usize,
    patch0: usize,
    upd0: usize,
    programs: Vec<u64>,
    spans: Vec<(u64, u64)>,
    height: u32,
    truncated: bool,
}

impl Open {
    fn absorb(
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
pub(super) struct Track {
    open: Vec<Open>,
    /// Entry states of finished recordings, kept so that a walk does not allocate one for each recording.
    #[allow(clippy::vec_box)]
    pool: Vec<Box<State>>,
    dep_list: Vec<DepSrc>,
    patches: Vec<Patch>,
    upd_log: Vec<Upd>,
    /// For each texture slot, the index in `dep_list` of the source that set it, or [`NO_DEP`].
    tex_dep: [u32; TEXTURE_SLOTS],
    /// Bit n is set when `tex_dep` of slot n is not [`NO_DEP`]. A template is only valid for the same provenance of the
    /// slots it reads, because only a slot from a deferred source needs a patch.
    prov: u128,
}

impl Track {
    pub(super) fn new() -> Self {
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

    fn reset(&mut self) {
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
    pub(super) fn tracking(&self) -> bool {
        !self.open.is_empty()
    }

    /// The walk reads the fields of `m`.
    pub(super) fn read(&mut self, m: &Bits) {
        if let Some(f) = self.open.last_mut() {
            f.needs.add_missing(m, &f.written);
        }
    }

    /// The walk reads one field.
    pub(super) fn read_bit(&mut self, bit: usize) {
        if let Some(f) = self.open.last_mut()
            && f.written.0[bit / 64] >> (bit % 64) & 1 == 0
        {
            f.needs.set(bit);
        }
    }

    /// The walk sets one field.
    pub(super) fn write(&mut self, bit: usize) {
        if let Some(f) = self.open.last_mut() {
            f.written.set(bit);
        }
    }

    /// The walk looks up `program`, which is in the slot `stage`.
    pub(super) fn program(&mut self, stage: usize, program: u64) {
        self.read_bit(program_bit(stage));
        if let Some(f) = self.open.last_mut()
            && !f.programs.contains(&program)
        {
            f.programs.push(program);
        }
    }

    /// The walk ended a run for a limit, so the recording is cut and cannot be a template.
    pub(super) fn truncated(&mut self) {
        if let Some(f) = self.open.last_mut() {
            f.truncated = true;
        }
    }

    /// The walk read the bytes `[start, end)` of a recording.
    pub(super) fn span(&mut self, start: u64, end: u64) {
        if let Some(f) = self.open.last_mut() {
            match f.spans.last_mut() {
                Some((_, e)) if *e == start => *e = end,
                _ => f.spans.push((start, end)),
            }
        }
    }

    /// A texture slot got a constant handle.
    pub(super) fn texture_set(&mut self, slot: usize) {
        self.write(texture_bit(slot));
        if let Some(d) = self.tex_dep.get_mut(slot) {
            *d = NO_DEP;
            self.prov &= !(1u128 << slot);
        }
    }

    /// A texture slot got the handle of a deferred source.
    pub(super) fn texture_deferred(&mut self, slot: usize, src: DepSrc) {
        let at = self.dep_list.len() as u32;
        self.dep_list.push(src);
        self.write(texture_bit(slot));
        if let Some(d) = self.tex_dep.get_mut(slot) {
            *d = at;
            self.prov |= 1u128 << slot;
        }
    }

    /// The deferred source for the handle `k` of a bind.
    pub(super) fn source(src: u64, k: u8, tex_base: u32, samp_base: u32, handle: u64) -> DepSrc {
        DepSrc { src, k, tex_base, samp_base, handle }
    }

    /// Logs an inline update at the current ends of the draws and dispatches.
    pub(super) fn update(&mut self, ext: &Ext, draws: usize, dispatches: usize) {
        if self.tracking() {
            self.upd_log.push(Upd { ext: *ext, draws: draws as u32, dispatches: dispatches as u32 });
        }
    }

    /// Logs the textures of a draw or a dispatch that came from deferred sources.
    pub(super) fn patches_of(&mut self, textures: &[TextureBind], idx: usize, dispatch: bool) {
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
            let dep = self.tex_dep.get(bank * 32 + t.slot as usize).copied().unwrap_or(NO_DEP);
            if dep != NO_DEP {
                let slot = (bank * 32 + t.slot as usize) as u8;
                self.patches.push(Patch { idx: idx as u32, dispatch, pos: pos as u16, dep, slot });
            }
        }
    }
}

/// What a run of fields in a [`Key`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Program,
    VertexBuffer,
    Ubo(u8),
    Sbo(u8),
    Attribute,
    Stream,
    Blend,
    Texture,
}

/// A run of `n` neighbouring fields of one kind, from the index `from`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Seg {
    kind: Kind,
    from: u16,
    n: u16,
}

/// The values that a template needs of its entry state. The fields come as runs of neighbouring slots, so that a lookup
/// compares them in tight loops. The scalar groups are compared as they are.
#[derive(Clone, Debug)]
pub(super) struct Key {
    segs: Vec<Seg>,
    /// The values of the runs, in order. A pair field (buffer address and size) takes two values.
    vals: Vec<u64>,
    scalars: Scalars,
    /// [`digest`] of `vals`. A state whose digest differs does not match, without a look at the fields.
    digest: u64,
}

/// Adds `v` to the digest `h` of the values of a key.
fn mix(h: u64, v: u64) -> u64 {
    (h.rotate_left(5) ^ v).wrapping_mul(0x517c_c1b7_2722_0a95)
}

fn texture_value(handle: u64, prov: u128, slot: usize) -> u64 {
    handle | u64::from(prov >> slot & 1 != 0) << 62
}

fn attribute_value(a: Option<(u32, u32, u32)>) -> u64 {
    a.map_or(0, |(f, o, s)| 1 << 63 | u64::from(f) | u64::from(o) << 8 | u64::from(s) << 40)
}

impl Key {
    /// The key for the fields of `needs`, with the values that `st` has. `prov` tells which texture slots came from
    /// a deferred source.
    fn of(needs: &Bits, st: &State, prov: u128) -> Self {
        let mut segs = Vec::new();
        let mut run = |kind: Kind, start: usize, len: usize| {
            let mut at = 0;
            while at < len {
                if needs.has(start + at) {
                    let from = at;
                    while at < len && needs.has(start + at) {
                        at += 1;
                    }
                    segs.push(Seg { kind, from: from as u16, n: (at - from) as u16 });
                } else {
                    at += 1;
                }
            }
        };
        run(Kind::Program, P, 6);
        run(Kind::VertexBuffer, VB, STREAMS);
        for bank in 0..super::BANKS as u8 {
            run(Kind::Ubo(bank), UBO + 16 * usize::from(bank), 16);
        }
        for bank in 0..super::BANKS as u8 {
            run(Kind::Sbo(bank), SBO + 16 * usize::from(bank), 16);
        }
        run(Kind::Attribute, ATTR, super::ATTRIBUTES);
        run(Kind::Stream, STREAM, STREAMS);
        run(Kind::Blend, BLEND, 8);
        run(Kind::Texture, TEX, TEXTURE_SLOTS);
        let mut vals = Vec::new();
        for s in &segs {
            let (from, n) = (usize::from(s.from), usize::from(s.n));
            match s.kind {
                Kind::Program => vals.extend_from_slice(&st.program[from..from + n]),
                Kind::VertexBuffer => st.vertex_buffer[from..from + n].iter().for_each(|(a, b)| vals.extend([*a, *b])),
                Kind::Ubo(b) => st.ubo[usize::from(b)][from..from + n].iter().for_each(|(a, s)| vals.extend([*a, u64::from(*s)])),
                Kind::Sbo(b) => st.sbuf[usize::from(b)][from..from + n].iter().for_each(|(a, s)| vals.extend([*a, u64::from(*s)])),
                Kind::Attribute => vals.extend(st.attribute[from..from + n].iter().map(|a| attribute_value(*a))),
                Kind::Stream => vals.extend(st.stream[from..from + n].iter().map(|(a, b)| u64::from(*a) | u64::from(*b) << 32)),
                Kind::Blend => vals.extend((from..from + n).map(|i| st.blends.raw(i))),
                Kind::Texture => vals.extend((from..from + n).map(|i| texture_value(st.texture[i], prov, i))),
            }
        }
        let groups = (0..32).filter(|g| needs.has(SC + g)).fold(0u32, |m, g| m | 1 << g);
        let digest = vals.iter().fold(0, |h, v| mix(h, *v));
        Self { segs, vals, scalars: Scalars::of(st, groups), digest }
    }

    /// The digest that `st` has for the fields of the key. It equals `self.digest` when `st` matches.
    fn digest_of(&self, st: &State, prov: u128) -> u64 {
        let mut h = 0;
        for s in &self.segs {
            let (from, n) = (usize::from(s.from), usize::from(s.n));
            for f in from..from + n {
                h = match s.kind {
                    Kind::Program => mix(h, st.program[f]),
                    Kind::VertexBuffer => mix(mix(h, st.vertex_buffer[f].0), st.vertex_buffer[f].1),
                    Kind::Ubo(b) => {
                        let (a, z) = st.ubo[usize::from(b)][f];
                        mix(mix(h, a), u64::from(z))
                    }
                    Kind::Sbo(b) => {
                        let (a, z) = st.sbuf[usize::from(b)][f];
                        mix(mix(h, a), u64::from(z))
                    }
                    Kind::Attribute => mix(h, attribute_value(st.attribute[f])),
                    Kind::Stream => mix(h, u64::from(st.stream[f].0) | u64::from(st.stream[f].1) << 32),
                    Kind::Blend => mix(h, st.blends.raw(f)),
                    Kind::Texture => mix(h, texture_value(st.texture[f], prov, f)),
                };
            }
        }
        h
    }

    /// The first field of the key that `st` does not have, for the `walkmiss` diagnostic.
    fn why_not(&self, st: &State, prov: u128) -> String {
        let mut k = 0;
        for s in &self.segs {
            let (from, n) = (usize::from(s.from), usize::from(s.n));
            for i in 0..n {
                let f = from + i;
                let (ok, width) = match s.kind {
                    Kind::Program => (self.vals[k + i] == st.program[f], 1),
                    Kind::VertexBuffer => {
                        let (a, b) = st.vertex_buffer[f];
                        (self.vals[k + 2 * i] == a && self.vals[k + 2 * i + 1] == b, 2)
                    }
                    Kind::Ubo(b) => {
                        let (a, z) = st.ubo[usize::from(b)][f];
                        (self.vals[k + 2 * i] == a && self.vals[k + 2 * i + 1] == u64::from(z), 2)
                    }
                    Kind::Sbo(b) => {
                        let (a, z) = st.sbuf[usize::from(b)][f];
                        (self.vals[k + 2 * i] == a && self.vals[k + 2 * i + 1] == u64::from(z), 2)
                    }
                    Kind::Attribute => (attribute_value(st.attribute[f]) == self.vals[k + i], 1),
                    Kind::Stream => {
                        let (a, b) = st.stream[f];
                        (u64::from(a) | u64::from(b) << 32 == self.vals[k + i], 1)
                    }
                    Kind::Blend => (st.blends.raw(f) == self.vals[k + i], 1),
                    Kind::Texture => (texture_value(st.texture[f], prov, f) == self.vals[k + i], 1),
                };
                let _ = width;
                if !ok {
                    return format!("{:?}[{f}]", s.kind);
                }
            }
            k += n * match s.kind {
                Kind::VertexBuffer | Kind::Ubo(_) | Kind::Sbo(_) => 2,
                _ => 1,
            };
        }
        if self.scalars.matches(st) { "nothing".into() } else { format!("scalars {:#x}", self.scalars.groups) }
    }

    /// Whether `st` has the values of the key.
    fn matches(&self, st: &State, prov: u128) -> bool {
        let pair = |at: usize, a: u64, b: u64| self.vals[at] == a && self.vals[at + 1] == b;
        let mut k = 0;
        for s in &self.segs {
            let (from, n) = (usize::from(s.from), usize::from(s.n));
            let (ok, width) = match s.kind {
                Kind::Program => (self.vals[k..k + n] == st.program[from..from + n], 1),
                Kind::VertexBuffer => {
                    let fields = &st.vertex_buffer[from..from + n];
                    (fields.iter().enumerate().all(|(i, (a, b))| pair(k + 2 * i, *a, *b)), 2)
                }
                Kind::Ubo(b) => {
                    let fields = &st.ubo[usize::from(b)][from..from + n];
                    (fields.iter().enumerate().all(|(i, (a, s))| pair(k + 2 * i, *a, u64::from(*s))), 2)
                }
                Kind::Sbo(b) => {
                    let fields = &st.sbuf[usize::from(b)][from..from + n];
                    (fields.iter().enumerate().all(|(i, (a, s))| pair(k + 2 * i, *a, u64::from(*s))), 2)
                }
                Kind::Attribute => {
                    let fields = &st.attribute[from..from + n];
                    (fields.iter().zip(&self.vals[k..k + n]).all(|(a, v)| attribute_value(*a) == *v), 1)
                }
                Kind::Stream => {
                    let fields = &st.stream[from..from + n];
                    (fields.iter().zip(&self.vals[k..k + n]).all(|((a, b), v)| u64::from(*a) | u64::from(*b) << 32 == *v), 1)
                }
                Kind::Blend => ((from..from + n).zip(&self.vals[k..k + n]).all(|(i, v)| st.blends.raw(i) == *v), 1),
                Kind::Texture => (
                    (from..from + n).zip(&self.vals[k..k + n]).all(|(i, v)| texture_value(st.texture[i], prov, i) == *v),
                    1,
                ),
            };
            if !ok {
                return false;
            }
            k += n * width;
        }
        self.scalars.matches(st)
    }
}

/// The templates of one handle.
#[derive(Default)]
struct Entry {
    /// Boxed, so that a hit takes a template out of the list and puts it back by moving a pointer.
    #[allow(clippy::vec_box)]
    list: Vec<Box<Template>>,
    /// The keys that this handle has in `Cache::by_start`.
    keys: Vec<(u64, u64)>,
}

/// Counts why a call missed the templates of its handle (`NXKIT_DIAG=walkmiss`), and prints the most common
/// reasons every 20000 misses.
fn walkmiss(why: String, templates: usize) {
    static MISSES: Mutex<Option<(u32, std::collections::HashMap<String, u32>)>> = Mutex::new(None);
    let mut g = MISSES.lock().unwrap_or_else(PoisonError::into_inner);
    let (n, map) = g.get_or_insert_with(Default::default);
    *n += 1;
    *map.entry(format!("{why} ({templates} kept)")).or_default() += 1;
    if *n % 20000 == 0 {
        let mut v: Vec<_> = map.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        let top: Vec<String> = v.iter().take(15).map(|(k, c)| format!("{k}: {c}")).collect();
        eprintln!("DIAG walkmiss after {n}: {}", top.join(" | "));
    }
}

/// The templates of all handles, and which memory each handle reads.
#[derive(Default)]
pub(super) struct Cache {
    entries: FastMap<u64, Entry>,
    /// `(start, handle)` to the end of each span of a recording that has templates.
    by_start: BTreeMap<(u64, u64), u64>,
    longest: u64,
    /// Handles that lost their templates because the game wrote their memory, since the last `take_counters`.
    pub(super) invalidations: u32,
    /// Templates dropped because a handle had too many, since the last `take_counters`.
    pub(super) evictions: u32,
    /// The evictions and hits of each handle since the last `clear`.
    churn: FastMap<u64, (u32, u32)>,
    /// Handles that are called with more states in a frame than [`MAX_PER_HANDLE`]: a model recording that runs for
    /// each instance with another uniform buffer. Their templates are dropped as fast as they are made, so a call
    /// walks them without one. The walk still tracks into the template of the caller.
    cold: FastMap<u64, ()>,
}

/// A handle with this many evictions turns cold when it had fewer than one hit per [`COLD_RATIO`] evictions.
const COLD_AFTER: u32 = 4 * MAX_PER_HANDLE as u32;
const COLD_RATIO: u32 = 8;

impl Cache {
    /// The number of templates that `handle` has.
    #[cfg(test)]
    pub(super) fn templates(&self, handle: u64) -> usize {
        self.entries.get(&handle).map_or(0, |e| e.list.len())
    }

    pub(super) fn clear(&mut self) {
        self.churn.clear();
        self.cold.clear();
        self.entries.clear();
        self.by_start.clear();
        self.longest = 0;
    }

    fn insert(&mut self, handle: u64, t: Box<Template>) {
        if self.entries.len() >= MAX_HANDLES && !self.entries.contains_key(&handle) {
            self.clear();
        }
        let e = self.entries.entry(handle).or_default();
        if e.list.len() >= MAX_PER_HANDLE {
            e.list.remove(0);
            self.evictions += 1;
            let c = self.churn.entry(handle).or_default();
            c.0 += 1;
            if c.0 >= COLD_AFTER && c.1 * COLD_RATIO < c.0 {
                self.cold.insert(handle, ());
                if let Some(e) = self.entries.remove(&handle) {
                    for k in e.keys {
                        self.by_start.remove(&k);
                    }
                }
                return;
            }
        }
        for &(start, end) in &t.spans {
            match self.by_start.entry((start, handle)) {
                std::collections::btree_map::Entry::Occupied(mut o) => {
                    if *o.get() < end {
                        o.insert(end);
                    }
                }
                std::collections::btree_map::Entry::Vacant(v) => {
                    v.insert(end);
                    e.keys.push((start, handle));
                }
            }
            self.longest = self.longest.max(end - start);
        }
        e.list.push(t);
    }

    fn take(&mut self, handle: u64, i: usize) -> Option<Box<Template>> {
        let e = self.entries.get_mut(&handle)?;
        (i < e.list.len()).then(|| e.list.remove(i))
    }

    fn put(&mut self, handle: u64, t: Box<Template>) {
        if let Some(e) = self.entries.get_mut(&handle) {
            e.list.push(t);
        }
    }

    fn drop_handle(&mut self, handle: u64) {
        if let Some(e) = self.entries.remove(&handle) {
            for k in e.keys {
                self.by_start.remove(&k);
            }
            self.invalidations += 1;
        }
    }

    /// Drops the templates of every handle that read a record in `[start, end)`.
    pub(super) fn invalidate(&mut self, start: u64, end: u64) {
        if end <= start {
            return;
        }
        let from = (start.saturating_sub(self.longest), 0);
        let hit: Vec<u64> = self
            .by_start
            .range(from..(end, 0))
            .filter(|(_, e)| **e > start)
            .map(|((_, h), _)| *h)
            .collect();
        for h in hit {
            self.drop_handle(h);
        }
    }
}

/// The ranges of guest memory that the game wrote, from threads that do not hold the walker.
///
/// The backend pushes a range when the front end learns about the write. The walker applies the ranges at the start of
/// its next [`Walker::submit`], before it reads any recording.
#[derive(Default)]
pub(crate) struct Invalidations {
    ranges: Mutex<Vec<(u64, u64)>>,
    dirty: AtomicBool,
}

impl Invalidations {
    /// Queues `[start, end)`.
    pub(crate) fn push(&self, start: u64, end: u64) {
        self.ranges.lock().unwrap_or_else(PoisonError::into_inner).push((start, end));
        self.dirty.store(true, Ordering::Release);
    }

    /// Moves the queued ranges to `out`.
    fn drain(&self, out: &mut Vec<(u64, u64)>) {
        if self.dirty.swap(false, Ordering::Acquire) {
            out.append(&mut self.ranges.lock().unwrap_or_else(PoisonError::into_inner));
        }
    }
}

/// Whether the unit tests run. A mismatch of the verification fails them, and only logs in the host.
fn under_test() -> bool {
    cfg!(test)
}

/// The bytes of a uniform buffer range, or none when no update touched it.
type Snapshot = Option<Arc<[u8]>>;

/// The uniform buffer ranges `(address, size)` of a template with their snapshots.
type RangeSnapshots = Vec<((u64, u32), Snapshot)>;

/// Whether two snapshots of a range are the same bytes.
fn same(a: &Snapshot, b: &Snapshot) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => Arc::ptr_eq(a, b) || a == b,
        _ => false,
    }
}

/// What a walk produced, for the comparison of a cached and an uncached walk.
struct Produced {
    draws: Vec<Arc<DrawDetail>>,
    clears: Vec<ClearDetail>,
    copies: Vec<CopyDetail>,
    dispatches: Vec<DispatchDetail>,
    reports: Vec<CounterReport>,
    updated: Vec<(u64, u64)>,
    state: Box<State>,
}

impl Produced {
    /// The first difference with `other`, as text.
    fn diff(&self, other: &Produced) -> Option<String> {
        fn first<T: PartialEq + std::fmt::Debug>(name: &str, a: &[T], b: &[T]) -> Option<String> {
            if a.len() != b.len() {
                return Some(format!("{name}: {} entries cached, {} walked", a.len(), b.len()));
            }
            let i = a.iter().zip(b).position(|(x, y)| x != y)?;
            Some(format!("{name}[{i}]: cached {:?}, walked {:?}", a[i], b[i]))
        }
        first("draws", &self.draws, &other.draws)
            .or_else(|| first("dispatches", &self.dispatches, &other.dispatches))
            .or_else(|| first("clears", &self.clears, &other.clears))
            .or_else(|| first("copies", &self.copies, &other.copies))
            .or_else(|| first("reports", &self.reports, &other.reports))
            .or_else(|| first("updated", &self.updated, &other.updated))
            .or_else(|| (self.state != other.state).then(|| "the state after the walk differs".to_owned()))
    }
}

impl Walker {
    /// Applies the memory writes that were queued since the last call.
    pub(super) fn drain_invalidations(&mut self) {
        let mut ranges = std::mem::take(&mut self.scratch_ranges);
        self.invalidations.drain(&mut ranges);
        for (start, end) in ranges.drain(..) {
            self.cache.invalidate(start, end);
        }
        self.scratch_ranges = ranges;
    }

    /// The queue of memory writes that the backend fills.
    pub(crate) fn invalidations(&self) -> Arc<Invalidations> {
        self.invalidations.clone()
    }

    fn marks(&self) -> Marks {
        Marks {
            draws: self.out.draws.len(),
            clears: self.out.clears.len(),
            copies: self.out.copies.len(),
            dispatches: self.out.dispatches.len(),
            reports: self.out.reports.len(),
        }
    }

    /// Walks one recording that `submit` was given. With the verification on, it walks it twice.
    pub(super) fn walk_top<M: GuestMem>(&mut self, mem: &mut M, st: &mut State, handle: u64) {
        if self.verify && self.cache_on {
            self.verify_top(mem, st, handle);
            return;
        }
        self.track.reset();
        self.run(mem, st, handle, 0);
    }

    fn produced(&self, marks: Marks, st: &State) -> Produced {
        Produced {
            draws: self.out.draws[marks.draws..].to_vec(),
            clears: self.out.clears[marks.clears..].to_vec(),
            copies: self.out.copies[marks.copies..].to_vec(),
            dispatches: self.out.dispatches[marks.dispatches..].to_vec(),
            reports: self.out.reports[marks.reports..].to_vec(),
            updated: self.updated.clone(),
            state: Box::new(st.clone()),
        }
    }

    /// Walks with the cache, takes the memory writes back, walks without it and compares. The uncached result stays.
    fn verify_top<M: GuestMem>(&mut self, mem: &mut M, st: &mut State, handle: u64) {
        let pre = st.clone();
        let updated = self.updated.clone();
        let snaps = self.snaps.get_mut().clone();
        let marks = self.marks();
        self.undo = Some(Vec::new());
        self.track.reset();
        self.run(mem, st, handle, 0);
        let undo = self.undo.take().unwrap_or_default();
        let cached = self.produced(marks, st);
        let counters = self.counters;
        for (host, old) in undo.iter().rev() {
            let _ = mem.write(*host, old);
        }
        self.updated = updated;
        *self.snaps.get_mut() = snaps;
        self.out.draws.truncate(marks.draws);
        self.out.clears.truncate(marks.clears);
        self.out.copies.truncate(marks.copies);
        self.out.dispatches.truncate(marks.dispatches);
        self.out.reports.truncate(marks.reports);
        *st = pre;
        self.bypass = true;
        self.track.reset();
        self.run(mem, st, handle, 0);
        self.bypass = false;
        self.counters = counters;
        let walked = self.produced(marks, st);
        if let Some(why) = cached.diff(&walked) {
            self.counters.verify_mismatches += 1;
            self.cache.clear();
            static LOGGED: AtomicBool = AtomicBool::new(false);
            if !LOGGED.swap(true, Ordering::Relaxed) {
                eprintln!("nxkit-gpu: walker template mismatch at handle {handle:#x}, using the uncached walk: {why}");
            }
            assert!(!under_test(), "walker template mismatch at handle {handle:#x}: {why}");
        }
    }

    /// Walks the recording at `handle` that a call record names, from a template when one fits.
    pub(super) fn run_nested<M: GuestMem>(&mut self, mem: &mut M, st: &mut State, handle: u64, depth: u32) {
        if !self.cache_on || self.bypass {
            self.walk_records(mem, st, handle, depth);
            return;
        }
        if let Some(i) = self.find(st, handle, depth)
            && let Some(mut t) = self.cache.take(handle, i)
        {
            self.counters.walk_hits += 1;
            self.counters.draws_from_template += t.draws.len() as u32;
            if let Some(c) = self.cache.churn.get_mut(&handle) {
                c.1 += 1;
            }
            if self.verify {
                self.play_verified(mem, st, handle, depth, &mut t);
            } else {
                self.play(mem, st, &mut t);
            }
            self.cache.put(handle, t);
            return;
        }
        let keep = !self.cache.cold.contains_key(&handle);
        if !keep {
            self.counters.cold_walks += 1;
        }
        self.build(mem, st, handle, depth, keep);
    }

    /// Plays the template, takes everything back, walks the recording without templates and compares. The uncached
    /// result stays. This finds the recording whose template is wrong, where [`Walker::verify_top`] only finds the
    /// top-level recording.
    fn play_verified<M: GuestMem>(&mut self, mem: &mut M, st: &mut State, handle: u64, depth: u32, t: &mut Template) {
        let pre = st.clone();
        let updated = self.updated.clone();
        let snaps = self.snaps.get_mut().clone();
        let marks = self.marks();
        let tracked = (self.track.dep_list.len(), self.track.patches.len(), self.track.upd_log.len(), self.track.tex_dep, self.track.prov);
        let parent = self.track.open.last().map(|p| (p.needs, p.written, p.programs.clone(), p.spans.clone(), p.height, p.truncated));
        let counters = self.counters;
        let outer = self.undo.replace(Vec::new());
        self.play(mem, st, t);
        let undo = self.undo.take().unwrap_or_default();
        let played = self.produced(marks, st);
        for (host, old) in undo.iter().rev() {
            let _ = mem.write(*host, old);
        }
        self.updated = updated;
        *self.snaps.get_mut() = snaps;
        self.out.draws.truncate(marks.draws);
        self.out.clears.truncate(marks.clears);
        self.out.copies.truncate(marks.copies);
        self.out.dispatches.truncate(marks.dispatches);
        self.out.reports.truncate(marks.reports);
        *st = pre;
        self.track.dep_list.truncate(tracked.0);
        self.track.patches.truncate(tracked.1);
        self.track.upd_log.truncate(tracked.2);
        self.track.tex_dep = tracked.3;
        self.track.prov = tracked.4;
        if let (Some(p), Some((needs, written, programs, spans, height, truncated))) = (self.track.open.last_mut(), parent) {
            (p.needs, p.written, p.programs, p.spans, p.height, p.truncated) = (needs, written, programs, spans, height, truncated);
        }
        self.undo = Some(Vec::new());
        self.bypass = true;
        self.walk_records(mem, st, handle, depth);
        self.bypass = false;
        let mut walk_undo = self.undo.take().unwrap_or_default();
        self.undo = outer;
        if let Some(outer) = &mut self.undo {
            outer.append(&mut walk_undo);
        }
        self.counters = counters;
        let walked = self.produced(marks, st);
        if let Some(why) = played.diff(&walked) {
            self.counters.verify_mismatches += 1;
            self.cache.clear();
            static LOGGED: AtomicBool = AtomicBool::new(false);
            if !LOGGED.swap(true, Ordering::Relaxed) {
                eprintln!("nxkit-gpu: walker template mismatch for the recording {handle:#x} at depth {depth}: {why}");
            }
            assert!(!under_test(), "walker template mismatch for the recording {handle:#x} at depth {depth}: {why}");
        }
    }

    /// The index of the template of `handle` that fits `st` at `depth`.
    fn find(&mut self, st: &State, handle: u64, depth: u32) -> Option<usize> {
        let epoch = self.programs.epoch();
        let e = self.cache.entries.get_mut(&handle)?;
        let mut stale = 0u64;
        let mut found = None;
        // The digest of `st` for the fields of template `shape`. The templates of a handle mostly read the same fields,
        // so it is computed once per distinct field set.
        let mut shape: Option<(usize, u64)> = None;
        for i in (0..e.list.len()).rev() {
            let d = match shape {
                Some((j, d)) if e.list[j].key.segs == e.list[i].key.segs => d,
                _ => {
                    let d = e.list[i].key.digest_of(st, self.track.prov);
                    shape = Some((i, d));
                    d
                }
            };
            let t = &mut e.list[i];
            if t.key.digest != d || depth + t.height > MAX_CALL_DEPTH {
                continue;
            }
            if t.epoch != epoch {
                if t.programs.iter().all(|(p, g)| self.programs.generation(*p) == *g) {
                    t.epoch = epoch;
                } else {
                    stale |= 1 << i;
                    continue;
                }
            }
            if t.key.matches(st, self.track.prov) {
                found = Some(i);
                break;
            }
        }
        if found.is_none() && stale == 0 && crate::diag("walkmiss") {
            if let Some(t) = e.list.last() {
                walkmiss(t.key.why_not(st, self.track.prov), e.list.len());
            }
        }
        if stale != 0 {
            let mut i = e.list.len();
            while i > 0 {
                i -= 1;
                if stale >> i & 1 != 0 {
                    e.list.remove(i);
                    found = found.map(|f| if f > i { f - 1 } else { f });
                }
            }
            if e.list.is_empty() {
                self.cache.drop_handle(handle);
                return None;
            }
        }
        found
    }

    /// Walks the recording and keeps a template of it.
    /// A cold handle (`keep` false) is tracked the same way, but its template is not made.
    fn build<M: GuestMem>(&mut self, mem: &mut M, st: &mut State, handle: u64, depth: u32, keep: bool) {
        let mut entry = self.track.pool.pop().unwrap_or_else(|| Box::new(State::default()));
        (*entry).clone_from(st);
        let open = Open {
            needs: Bits::NONE,
            written: Bits::NONE,
            entry,
            prov: self.track.prov,
            marks: self.marks(),
            dep0: self.track.dep_list.len(),
            patch0: self.track.patches.len(),
            upd0: self.track.upd_log.len(),
            programs: Vec::new(),
            spans: Vec::new(),
            height: 0,
            truncated: false,
        };
        self.track.open.push(open);
        self.walk_records(mem, st, handle, depth);
        let Some(f) = self.track.open.pop() else {
            return;
        };
        self.counters.walk_misses += 1;
        if let Some(p) = self.track.open.last_mut() {
            p.absorb(&f.needs, &f.written, f.programs.iter().copied(), &f.spans, f.height, f.truncated);
        }
        if f.truncated || !keep {
            self.track.pool.push(f.entry);
            return;
        }
        let t = self.template(&f, st);
        self.track.pool.push(f.entry);
        self.cache.insert(handle, t);
    }

    fn template(&self, f: &Open, st: &State) -> Box<Template> {
        let key = Key::of(&f.needs, &f.entry, f.prov);
        let m = f.marks;
        let (draws, dispatches) = (m.draws as u32, m.dispatches as u32);
        let draws_v = self.out.draws[m.draws..].to_vec();
        let dispatches_v = self.out.dispatches[m.dispatches..].to_vec();
        let updates: Vec<Upd> = self.track.upd_log[f.upd0..]
            .iter()
            .map(|u| Upd { ext: u.ext, draws: u.draws - draws, dispatches: u.dispatches - dispatches })
            .collect();
        let rebase = |p: &Patch| Patch { idx: p.idx - if p.dispatch { dispatches } else { draws }, ..*p };
        let patches = self.track.patches[f.patch0..]
            .iter()
            .filter(|p| p.dep as usize >= f.dep0)
            .map(|p| Patch { dep: p.dep - f.dep0 as u32, ..rebase(p) })
            .collect();
        let inherited = self.track.patches[f.patch0..]
            .iter()
            .filter(|p| (p.dep as usize) < f.dep0)
            .map(rebase)
            .collect();
        let mut delta = Vec::new();
        let mut groups = 0u32;
        for bit in f.written.iter() {
            let d = match bit {
                P..VB => Delta::Program((bit - P) as u8, st.program[bit - P]),
                VB..UBO => Delta::VertexBuffer((bit - VB) as u8, st.vertex_buffer[bit - VB]),
                UBO..SBO => {
                    let i = bit - UBO;
                    Delta::Ubo(i as u8, st.ubo[i / 16][i % 16])
                }
                SBO..ATTR => {
                    let i = bit - SBO;
                    Delta::Sbo(i as u8, st.sbuf[i / 16][i % 16])
                }
                ATTR..STREAM => Delta::Attribute((bit - ATTR) as u8, st.attribute[bit - ATTR]),
                STREAM..BLEND => Delta::Stream((bit - STREAM) as u8, st.stream[bit - STREAM]),
                BLEND..TEX => Delta::Blend((bit - BLEND) as u8, st.blends.raw(bit - BLEND)),
                TEX..SC => {
                    let slot = bit - TEX;
                    match self.track.tex_dep[slot] {
                        d if d != NO_DEP && d as usize >= f.dep0 => Delta::TextureDeferred(slot as u8, d - f.dep0 as u32),
                        _ => Delta::Texture(slot as u8, st.texture[slot]),
                    }
                }
                _ => {
                    groups |= 1 << (bit - SC);
                    continue;
                }
            };
            delta.push(d);
        }
        let ranges = if updates.is_empty() { ranges_of(&draws_v, &dispatches_v) } else { Vec::new() };
        let mut spans = f.spans.clone();
        spans.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::with_capacity(spans.len());
        for (s, e) in spans {
            match merged.last_mut() {
                Some((_, le)) if s <= *le => *le = (*le).max(e),
                _ => merged.push((s, e)),
            }
        }
        Box::new(Template {
            needs: f.needs,
            written: f.written,
            key,
            height: f.height,
            epoch: self.programs.epoch(),
            programs: f.programs.iter().map(|p| (*p, self.programs.generation(*p))).collect(),
            spans: merged,
            draws: draws_v,
            dispatches: dispatches_v,
            clears: self.out.clears[m.clears..].to_vec(),
            copies: self.out.copies[m.copies..].to_vec(),
            reports: self.out.reports[m.reports..].to_vec(),
            updates,
            deferred: self.track.dep_list[f.dep0..].to_vec(),
            patches,
            inherited,
            delta,
            scalars: (groups != 0).then(|| Box::new(Scalars::of(st, groups))),
            ranges,
        })
    }

    /// Plays the template: the outputs, the memory writes, the state it leaves, and the logs for the recording that
    /// calls it.
    fn play<M: GuestMem>(&mut self, mem: &mut M, st: &mut State, t: &mut Template) {
        let dep_base = self.track.dep_list.len();
        let mut moved = false;
        for d in &mut t.deferred {
            let handle = deferred_handle(&*mem, d.src, d.k, d.tex_base, d.samp_base).map_or(0, |(_, _, h)| h);
            moved |= handle != d.handle;
            self.track.dep_list.push(DepSrc { handle, ..*d });
        }
        let (d0, p0) = (self.out.draws.len(), self.out.dispatches.len());
        self.out.clears.extend_from_slice(&t.clears);
        self.out.copies.extend_from_slice(&t.copies);
        self.out.reports.extend_from_slice(&t.reports);
        if t.updates.is_empty() && self.ranges_hold(mem, t) {
            self.out.draws.extend(t.draws.iter().cloned());
            self.out.dispatches.extend(t.dispatches.iter().cloned());
        } else {
            self.play_in_order(mem, t);
        }
        if moved {
            self.patch_textures(t, dep_base, d0, p0);
        }
        if self.track.tracking() {
            for u in &t.updates {
                self.track.update(&u.ext, d0 + u.draws as usize, p0 + u.dispatches as usize);
            }
            for p in &t.patches {
                let base = if p.dispatch { p0 } else { d0 };
                self.track.patches.push(Patch {
                    idx: (base + p.idx as usize) as u32,
                    dep: (dep_base as u32) + p.dep,
                    ..*p
                });
            }
            for p in &t.inherited {
                let base = if p.dispatch { p0 } else { d0 };
                let dep = self.track.tex_dep.get(usize::from(p.slot)).copied().unwrap_or(NO_DEP);
                if dep != NO_DEP {
                    self.track.patches.push(Patch { idx: (base + p.idx as usize) as u32, dep, ..*p });
                }
            }
        }
        self.apply_delta(st, t, dep_base);
        if let Some(parent) = self.track.open.last_mut() {
            parent.absorb(&t.needs, &t.written, t.programs.iter().map(|(p, _)| *p), &t.spans, t.height, false);
        }
    }

    /// Whether the uniform buffer ranges of a template without updates still have the snapshots that its draws hold.
    fn ranges_hold<M: GuestMem>(&self, mem: &M, t: &Template) -> bool {
        t.ranges.iter().all(|((a, s), snap)| same(&self.snapshot(mem, *a, *s), snap))
    }

    /// Plays the draws and dispatches in the order of the walk, with the inline updates between them. A draw whose
    /// snapshots no longer match gets new ones, and the template keeps the new draw.
    fn play_in_order<M: GuestMem>(&mut self, mem: &mut M, t: &mut Template) {
        let (mut d, mut p) = (0usize, 0usize);
        let n = t.updates.len();
        for u in 0..=n {
            let (de, pe) = match t.updates.get(u) {
                Some(up) => (up.draws as usize, up.dispatches as usize),
                None => (t.draws.len(), t.dispatches.len()),
            };
            for k in d..de {
                let a = self.fresh_draw(mem, &t.draws[k]);
                t.draws[k] = a.clone();
                self.out.draws.push(a);
            }
            for k in p..pe {
                let x = self.fresh_dispatch(mem, &t.dispatches[k]);
                t.dispatches[k] = x.clone();
                self.out.dispatches.push(x);
            }
            (d, p) = (de, pe);
            if let Some(up) = t.updates.get(u) {
                let mut bytes = std::mem::take(&mut self.update_bytes);
                self.write_update(mem, &up.ext, &mut bytes);
                self.update_bytes = bytes;
            }
        }
        if t.updates.is_empty() {
            t.ranges = ranges_of(&t.draws, &t.dispatches);
        }
    }

    /// The draw with the snapshots that its uniform buffers have now.
    fn fresh_draw<M: GuestMem>(&self, mem: &M, a: &Arc<DrawDetail>) -> Arc<DrawDetail> {
        if a.ubos.iter().all(|u| same(&self.snapshot(mem, u.address, u.size), &u.snapshot)) {
            return a.clone();
        }
        let mut d = (**a).clone();
        for u in &mut d.ubos {
            u.snapshot = self.snapshot(mem, u.address, u.size);
        }
        Arc::new(d)
    }

    /// The dispatch with the snapshots that its uniform buffers have now.
    fn fresh_dispatch<M: GuestMem>(&self, mem: &M, x: &DispatchDetail) -> DispatchDetail {
        let mut x = x.clone();
        for u in &mut x.ubos {
            u.snapshot = self.snapshot(mem, u.address, u.size);
        }
        x
    }

    /// Gives the draws and dispatches that took a handle from a deferred source that changed the new handle.
    fn patch_textures(&mut self, t: &mut Template, dep_base: usize, d0: usize, p0: usize) {
        for p in &t.patches {
            let handle = self.track.dep_list[dep_base + p.dep as usize].handle;
            let pos = usize::from(p.pos);
            if p.dispatch {
                let at = p0 + p.idx as usize;
                if let Some(x) = self.out.dispatches.get_mut(at).filter(|x| x.textures[pos].handle != handle) {
                    x.textures[pos].handle = handle;
                    t.dispatches[p.idx as usize].textures[pos].handle = handle;
                }
            } else {
                let at = d0 + p.idx as usize;
                if let Some(a) = self.out.draws.get_mut(at).filter(|a| a.textures[pos].handle != handle) {
                    let mut d = (**a).clone();
                    d.textures[pos].handle = handle;
                    *a = Arc::new(d);
                    t.draws[p.idx as usize] = a.clone();
                }
            }
        }
        for (i, d) in t.deferred.iter_mut().enumerate() {
            d.handle = self.track.dep_list[dep_base + i].handle;
        }
    }

    /// Sets the fields that the recording set to the values it left.
    fn apply_delta(&mut self, st: &mut State, t: &Template, dep_base: usize) {
        for d in &t.delta {
            match *d {
                Delta::Program(i, v) => st.program[usize::from(i)] = v,
                Delta::VertexBuffer(i, v) => st.vertex_buffer[usize::from(i)] = v,
                Delta::Ubo(i, v) => st.ubo[usize::from(i) / 16][usize::from(i) % 16] = v,
                Delta::Sbo(i, v) => st.sbuf[usize::from(i) / 16][usize::from(i) % 16] = v,
                Delta::Attribute(i, v) => st.attribute[usize::from(i)] = v,
                Delta::Stream(i, v) => st.stream[usize::from(i)] = v,
                Delta::Blend(i, v) => st.blends.set_raw(usize::from(i), v),
                Delta::Texture(i, v) => {
                    st.texture[usize::from(i)] = v;
                    self.track.tex_dep[usize::from(i)] = NO_DEP;
                    self.track.prov &= !(1u128 << i);
                }
                Delta::TextureDeferred(i, rel) => {
                    let at = dep_base + rel as usize;
                    st.texture[usize::from(i)] = self.track.dep_list[at].handle;
                    self.track.tex_dep[usize::from(i)] = at as u32;
                    self.track.prov |= 1u128 << i;
                }
            }
        }
        if let Some(s) = &t.scalars {
            s.apply(st);
        }
    }
}

/// The distinct uniform buffer ranges of the draws and dispatches, each with its snapshot.
fn ranges_of(draws: &[Arc<DrawDetail>], dispatches: &[DispatchDetail]) -> RangeSnapshots {
    let mut seen: FastMap<(u64, u32), usize> = FastMap::default();
    let mut out = RangeSnapshots::new();
    let ubos = draws
        .iter()
        .flat_map(|d| d.ubos.iter())
        .chain(dispatches.iter().flat_map(|d| d.ubos.iter()));
    for u in ubos {
        if seen.insert((u.address, u.size), out.len()).is_none() {
            out.push(((u.address, u.size), u.snapshot.clone()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detail::FrameDetail;
    use nxkit_metal::AddressMap;
    use nxkit_nvn::backend::{ShaderKey, StageKey};
    use nxkit_nvn::cmdbuf::CommandHandle;
    use nxkit_nvn::ext::{DrawKind, ExtCode, Setting};
    use nxkit_nvn::mem::VecMem;
    use nxkit_nvn::record::Record;

    const HOST: u64 = 0x7000_0000;
    const GPU: u64 = 0x15_0400_8000;
    const SIZE: u64 = 1 << 20;
    /// The first free address for recordings.
    const CODE: u64 = HOST + 0x8_0000;
    const PROG: u64 = GPU + 0x9000;
    const UBO_AT: u64 = GPU + 0x1000;
    const SRC: u64 = HOST + 0x3000;

    fn ext(code: ExtCode, arg: u16, words: &[u32]) -> Record {
        Record::Ext(words.iter().fold(Ext::new(code, arg), |x, w| x.u32(*w)))
    }

    fn halves(v: u64) -> [u32; 2] {
        [v as u32, (v >> 32) as u32]
    }

    fn program(addr: u64) -> Record {
        let [lo, hi] = halves(addr);
        ext(ExtCode::Program, 0, &[lo, hi, 0b11])
    }

    fn viewport(width: f32) -> Record {
        ext(ExtCode::Viewport, 0, &[0, 0, width.to_bits(), 90f32.to_bits()])
    }

    fn ubo(slot: u16, addr: u64, size: u32) -> Record {
        let [lo, hi] = halves(addr);
        ext(ExtCode::UniformBuffer, slot, &[lo, hi, size, 0])
    }

    fn vertex_buffer(stream: u16, addr: u64, size: u32) -> Record {
        let [lo, hi] = halves(addr);
        ext(ExtCode::VertexBuffer, stream, &[lo, hi, size, 0])
    }

    fn attribute(location: u32, format: u32, stream: u32) -> Record {
        ext(ExtCode::VertexState, 0, &[location, 1, format | stream << 27])
    }

    fn stream(index: u32, stride: u32) -> Record {
        ext(ExtCode::VertexState, 1, &[index, 1, stride, 0])
    }

    fn draw() -> Record {
        ext(ExtCode::Draw, DrawKind::Arrays as u16, &[4, 0, 3])
    }

    fn dispatch() -> Record {
        ext(ExtCode::Dispatch, 0, &[2, 3, 4])
    }

    fn clear() -> Record {
        ext(ExtCode::ClearColor, 0, &[0xf, 1f32.to_bits(), 0, 0, 0])
    }

    fn update(buffer: u64, size: u32, offset: u32, value: u32) -> Record {
        let [lo, hi] = halves(buffer);
        ext(ExtCode::UpdateUniform, 4, &[lo, hi, size, 0, offset, 0, value])
    }

    fn blend(target: u32, factor: u32) -> Record {
        let v = u64::from(target) | u64::from(factor) << 3 | u64::from(factor) << 10;
        ext(ExtCode::State, 0, &[v as u32, (v >> 32) as u32])
    }

    fn stencil_ref(face: u32, value: u32) -> Record {
        ext(ExtCode::Setting, Setting::StencilRef as u16, &[face, value])
    }

    fn deferred(first: u8, count: u8, src: u64) -> Record {
        Record::BindTexturesDeferred { first, count, src, tex_base: 0, samp_base: 0 }
    }

    /// A model recording: its own buffers and vertex layout, then `draws` draws.
    fn model(buffer: u64, draws: usize) -> Vec<Record> {
        let mut r = vec![
            ubo(0, buffer, 64),
            vertex_buffer(0, GPU + 0x2000, 256),
            attribute(0, 0x0a, 0),
            stream(0, 16),
        ];
        r.extend((0..draws).map(|_| draw()));
        r
    }

    /// A walker over a block of guest memory.
    struct Rig {
        mem: VecMem,
        w: Walker,
        at: u64,
    }

    impl Rig {
        fn new() -> Self {
            let map = AddressMap::new(HOST, GPU, SIZE).unwrap();
            let mut w = Walker::new(Default::default(), map);
            w.program_set(PROG, &keys(1, 2));
            Self { mem: VecMem::new(HOST, SIZE as usize), w, at: CODE }
        }

        fn put_at(&mut self, at: u64, recs: &[Record]) -> u64 {
            let mut p = at;
            for rec in recs.iter().chain([&Record::End]) {
                let bytes = rec.encode().unwrap();
                self.mem.write(p, bytes.as_bytes()).unwrap();
                p += bytes.len() as u64;
            }
            at
        }

        fn put(&mut self, recs: &[Record]) -> u64 {
            let at = self.at;
            self.put_at(at, recs);
            self.at += 0x1000;
            at
        }

        fn frame(&mut self, handles: &[u64]) -> (FrameDetail, super::super::WalkCounters) {
            let h: Vec<CommandHandle> = handles.iter().map(|h| CommandHandle(*h)).collect();
            self.w.submit(&mut self.mem, 1, &h);
            let c = self.w.take_counters();
            (self.w.take(), c)
        }

        fn bytes(&self, at: u64, len: usize) -> Vec<u8> {
            let mut b = vec![0; len];
            self.mem.read(at, &mut b).unwrap();
            b
        }
    }

    /// Two walkers over the same guest memory: `cached` plays templates, `plain` walks every record.
    struct Pair {
        cached: Rig,
        plain: Rig,
    }

    impl Pair {
        /// With `verify`, the cached walker also compares itself with an uncached walk, and returns the uncached result.
        fn new(verify: bool) -> Self {
            let mut cached = Rig::new();
            cached.w.verify = verify;
            let mut plain = Rig::new();
            plain.w.cache_on = false;
            plain.w.verify = false;
            Self { cached, plain }
        }

        fn put(&mut self, recs: &[Record]) -> u64 {
            let h = self.cached.put(recs);
            assert_eq!(h, self.plain.put(recs));
            h
        }

        fn put_at(&mut self, at: u64, recs: &[Record]) {
            self.cached.put_at(at, recs);
            self.plain.put_at(at, recs);
        }

        fn write(&mut self, at: u64, bytes: &[u8]) {
            self.cached.mem.write(at, bytes).unwrap();
            self.plain.mem.write(at, bytes).unwrap();
        }

        /// The game wrote the command memory `[at, at + len)` and told the backend.
        fn rewritten(&mut self, at: u64, len: u64) {
            self.cached.w.invalidations().push(at, at + len);
        }

        /// Walks the frame with both and compares what they produced, the memory they wrote and the counters.
        fn frame(&mut self, handles: &[u64]) -> (FrameDetail, super::super::WalkCounters) {
            let (a, counters) = self.cached.frame(handles);
            let (b, _) = self.plain.frame(handles);
            assert_eq!(a.draws.len(), b.draws.len());
            for (i, (x, y)) in a.draws.iter().zip(&b.draws).enumerate() {
                assert!(x == y, "draw {i}: {}", differences(x, y));
            }
            assert_eq!(a.dispatches.len(), b.dispatches.len());
            for (i, (x, y)) in a.dispatches.iter().zip(&b.dispatches).enumerate() {
                assert!(x == y, "dispatch {i}: {x:?} against {y:?}");
            }
            assert_eq!(a.clears, b.clears);
            assert_eq!(a.copies, b.copies);
            assert_eq!(a.reports, b.reports);
            assert_eq!(self.cached.bytes(HOST, 0x4000), self.plain.bytes(HOST, 0x4000));
            (a, counters)
        }
    }

    /// The names of the fields in which two draws differ.
    fn differences(a: &DrawDetail, b: &DrawDetail) -> String {
        let mut out = Vec::new();
        macro_rules! check {
            ($($f:ident),*) => { $(if a.$f != b.$f { out.push(format!("{}: {:?} against {:?}", stringify!($f), a.$f, b.$f)); })* };
        }
        check!(program, blend, write_masks, depth_stencil, raster, topology, attributes, streams, args, ubos, sbufs, dynamic);
        if a.textures != b.textures {
            let slots: Vec<String> = a
                .textures
                .iter()
                .zip(&b.textures)
                .filter(|(x, y)| x != y)
                .map(|(x, y)| format!("slot {} {} against {}", x.slot, x.handle, y.handle))
                .collect();
            out.push(format!("textures ({} against {}): {}", a.textures.len(), b.textures.len(), slots.join(", ")));
        }
        out.join("; ")
    }

    fn keys(vertex: u8, fragment: u8) -> [StageKey; 2] {
        let stage = |index: u8, k: u8| StageKey { index, key: Some(ShaderKey([k; 8])), code: 0, code_len: 0 };
        [stage(0, vertex), stage(1, fragment)]
    }

    fn for_both(scenario: impl Fn(&mut Pair)) {
        for verify in [false, true] {
            scenario(&mut Pair::new(verify));
        }
    }

    #[test]
    fn a_nested_recording_plays_from_its_template() {
        for_both(|p| {
            let child = p.put(&model(UBO_AT, 2));
            let top = p.put(&[program(PROG), viewport(100.0), Record::Call(child), Record::Call(child), draw(), viewport(200.0), Record::Call(child)]);
            let (d, c) = p.frame(&[top]);
            assert_eq!(d.draws.len(), 7);
            assert_eq!((c.walk_hits, c.walk_misses, c.draws_from_template), (1, 2, 2));
            let (d, c) = p.frame(&[top]);
            assert_eq!(d.draws.len(), 7);
            assert_eq!((c.walk_hits, c.walk_misses, c.draws_from_template), (3, 0, 6));
            assert_eq!(p.cached.w.cache.templates(child), 2);
            assert!(c.records < 12, "a hit does not read the records of the callee: {}", c.records);
            assert_eq!(d.draws[0].dynamic.viewport[2], 100.0);
            assert_eq!(d.draws[6].dynamic.viewport[2], 200.0);
        });
    }

    #[test]
    fn a_hit_shares_the_draws_with_its_template() {
        let mut p = Pair::new(false);
        let child = p.put(&model(UBO_AT, 3));
        let top = p.put(&[program(PROG), Record::Call(child)]);
        let (a, _) = p.frame(&[top]);
        let (b, _) = p.frame(&[top]);
        assert!(a.draws.iter().zip(&b.draws).all(|(x, y)| Arc::ptr_eq(x, y)));
    }

    #[test]
    fn inherited_state_picks_the_template() {
        const OTHER: u64 = GPU + 0xa000;
        for_both(|p| {
            for rig in [&mut p.cached, &mut p.plain] {
                rig.w.program_set(OTHER, &keys(5, 6));
            }
            let child = p.put(&[draw(), clear(), dispatch()]);
            let first = vec![program(PROG), viewport(10.0), Record::BindTexture { slot: 3, handle: 7 }, Record::Call(child)];
            let calls = [
                first.clone(),
                first.clone(),
                vec![viewport(20.0), Record::Call(child)],
                vec![Record::BindTexture { slot: 3, handle: 8 }, Record::Call(child)],
                vec![program(OTHER), Record::Call(child)],
                first,
            ];
            let tops: Vec<u64> = calls.iter().map(|r| p.put(r)).collect();
            let (d, c) = p.frame(&tops);
            assert_eq!(d.draws.len(), 6);
            assert_eq!((c.walk_hits, c.walk_misses), (2, 4));
            assert_ne!(d.draws[0], d.draws[2]);
            assert_eq!(&*d.draws[4].program.vertex, "0505050505050505");
            let (_, c) = p.frame(&tops);
            assert_eq!((c.walk_hits, c.walk_misses), (6, 0));
            assert_eq!(p.cached.w.cache.templates(child), 4);
        });
    }

    #[test]
    fn state_that_the_callee_sets_stays_set_for_the_caller() {
        for_both(|p| {
            let child = p.put(&[
                program(PROG),
                viewport(33.0),
                ubo(2, UBO_AT, 32),
                blend(2, 5),
                stencil_ref(2, 12),
                ext(ExtCode::Setting, Setting::CopyRowStride as u16, &[256, 0]),
                Record::BindTexture { slot: 40, handle: 11 },
            ]);
            let top = p.put(&[Record::Call(child), draw(), dispatch()]);
            for _ in 0..3 {
                let (d, _) = p.frame(&[top]);
                assert_eq!(d.draws[0].dynamic.viewport[2], 33.0);
                assert_eq!(d.draws[0].dynamic.stencil_ref[1], 12);
                assert_eq!(d.draws[0].blend[2].src_rgb, 5);
            }
        });
    }

    #[test]
    fn mutated_deferred_handles_patch_the_textures_of_the_draws() {
        for_both(|p| {
            let set = |p: &mut Pair, a: u64, b: u64, c: u64| {
                let bytes: Vec<u8> = [a, b, c].iter().flat_map(|v| v.to_le_bytes()).collect();
                p.write(SRC, &bytes);
            };
            set(p, 5, 6, 7);
            let mut m = vec![deferred(0, 2, SRC), deferred(64, 1, SRC + 16)];
            m.extend(model(UBO_AT, 2));
            m.push(dispatch());
            let child = p.put(&m);
            let mid = p.put(&[program(PROG), Record::Call(child), draw()]);
            let top = p.put(&[Record::Call(mid), Record::Call(mid)]);
            let (d, c) = p.frame(&[top]);
            assert_eq!((c.walk_hits, c.walk_misses), (1, 2));
            assert_eq!(d.draws[0].textures.iter().map(|t| t.handle).collect::<Vec<_>>(), [5, 6]);
            for values in [(5, 6, 7), (5, 6, 7), (9, 6, 7), (9, 10, 11), (9, 10, 11), (0, 0, 0), (5, 6, 7)] {
                set(p, values.0, values.1, values.2);
                let (d, c) = p.frame(&[top]);
                assert_eq!(c.walk_hits, 2, "{values:?}");
                assert_eq!(d.draws[3].textures.iter().map(|t| t.handle).collect::<Vec<_>>(), [values.0, values.1]);
                assert_eq!(d.draws[2].textures.iter().map(|t| t.handle).collect::<Vec<_>>(), [values.0, values.1]);
                assert_eq!(d.dispatches[1].textures[0].handle, values.2);
            }
        });
    }

    #[test]
    fn a_rewritten_recording_drops_its_templates_and_those_of_its_callers() {
        for_both(|p| {
            let child = p.put(&model(UBO_AT, 2));
            let mid = p.put(&[program(PROG), Record::Call(child)]);
            let top = p.put(&[Record::Call(mid)]);
            p.frame(&[top]);
            let (_, c) = p.frame(&[top]);
            assert_eq!((c.walk_hits, c.walk_misses), (1, 0));
            p.put_at(child, &model(UBO_AT, 5));
            p.rewritten(child, 0x100);
            let (d, c) = p.frame(&[top]);
            assert_eq!(d.draws.len(), 5);
            assert_eq!(c.invalidations, 2);
            assert_eq!((c.walk_hits, c.walk_misses), (0, 2));
            let (d, c) = p.frame(&[top]);
            assert_eq!((d.draws.len(), c.walk_hits), (5, 1));
            // A write elsewhere changes nothing.
            p.rewritten(child + 0x800, 0x10);
            assert_eq!(p.frame(&[top]).1.walk_hits, 1);
        });
    }

    #[test]
    fn a_partial_update_with_a_rewritten_tail_gets_a_new_snapshot() {
        for_both(|p| {
            let mut m = vec![ubo(0, UBO_AT, 64), update(UBO_AT, 64, 16, 0xaabbccdd)];
            m.extend([vertex_buffer(0, GPU + 0x2000, 256), attribute(0, 0x0a, 0), stream(0, 16), draw()]);
            let child = p.put(&m);
            let top = p.put(&[program(PROG), Record::Call(child), Record::Call(child)]);
            for round in 0u8..4 {
                // The game rewrites the unchanged tail of the buffer before each submit and says nothing about it.
                p.write(HOST + 0x1000 + 48, &[round + 1; 8]);
                let (d, c) = p.frame(&[top]);
                assert_eq!(c.walk_hits, if round == 0 { 1 } else { 2 });
                for draw in &d.draws {
                    let snap = draw.ubos[0].snapshot.as_ref().unwrap();
                    assert_eq!(snap.len(), 64);
                    assert_eq!(&snap[16..20], &0xaabbccddu32.to_le_bytes());
                    assert_eq!(&snap[48..56], &[round + 1; 8]);
                }
            }
        });
    }

    #[test]
    fn an_update_from_another_recording_gives_a_snapshot_to_a_cached_draw() {
        for_both(|p| {
            let reader = p.put(&model(UBO_AT + 0x100, 1));
            let writer = p.put(&[update(UBO_AT + 0x100, 64, 0, 0x01020304)]);
            let top = p.put(&[program(PROG), Record::Call(reader)]);
            let later = p.put(&[program(PROG), Record::Call(writer), Record::Call(reader)]);
            let (d, _) = p.frame(&[top]);
            assert!(d.draws[0].ubos[0].snapshot.is_none());
            let (d, c) = p.frame(&[top]);
            assert_eq!(c.walk_hits, 1);
            assert!(d.draws[0].ubos[0].snapshot.is_none());
            let (d, c) = p.frame(&[later]);
            assert_eq!(c.walk_hits, 1);
            assert_eq!(&d.draws[0].ubos[0].snapshot.as_ref().unwrap()[..4], &0x01020304u32.to_le_bytes());
            p.frame(&[top, later, top]);
        });
    }

    #[test]
    fn an_update_between_draws_gives_each_draw_the_bytes_it_saw() {
        for_both(|p| {
            let mut m = model(UBO_AT, 1);
            m.extend([update(UBO_AT, 64, 0, 1), draw(), dispatch(), update(UBO_AT, 64, 0, 2), draw()]);
            let child = p.put(&m);
            let top = p.put(&[program(PROG), Record::Call(child)]);
            for round in 0..3 {
                let (d, _) = p.frame(&[top]);
                let seen = |i: usize| d.draws[i].ubos[0].snapshot.as_ref().map(|s| s[0]);
                // After the first frame the range is in the history of updates, and the first draw sees the last write.
                let before = if round == 0 { None } else { Some(2) };
                assert_eq!((seen(0), seen(1), seen(2)), (before, Some(1), Some(2)));
            }
        });
    }

    #[test]
    fn program_changes_make_templates_stale() {
        for_both(|p| {
            let child = p.put(&model(UBO_AT, 2));
            let top = p.put(&[program(PROG), Record::Call(child)]);
            p.frame(&[top]);
            assert_eq!(p.frame(&[top]).1.walk_hits, 1);
            for rig in [&mut p.cached, &mut p.plain] {
                rig.w.program_set(PROG, &keys(3, 4));
            }
            let (d, c) = p.frame(&[top]);
            assert_eq!((c.walk_hits, c.walk_misses), (0, 1));
            assert_eq!(&*d.draws[0].program.vertex, "0303030303030303");
            assert_eq!(p.frame(&[top]).1.walk_hits, 1);
            // The same stages again change nothing.
            for rig in [&mut p.cached, &mut p.plain] {
                rig.w.program_set(PROG, &keys(3, 4));
            }
            assert_eq!(p.frame(&[top]).1.walk_hits, 1);
            for rig in [&mut p.cached, &mut p.plain] {
                rig.w.program_released(PROG);
            }
            let (d, c) = p.frame(&[top]);
            assert_eq!(c.walk_hits, 0);
            assert_eq!(&*d.draws[0].program.vertex, "");
        });
    }

    #[test]
    fn a_handle_keeps_at_most_max_per_handle_templates() {
        const N: usize = MAX_PER_HANDLE + 2;
        for_both(|p| {
            let child = p.put(&[draw()]);
            let calls: Vec<Record> = [program(PROG)]
                .into_iter()
                .chain((0..N).flat_map(|i| [viewport(10.0 + i as f32), Record::Call(child)]))
                .collect();
            let top = p.put(&calls);
            let (_, c) = p.frame(&[top]);
            assert_eq!((c.walk_misses, c.evictions), (N as u32, 2));
            assert_eq!(p.cached.w.cache.templates(child), MAX_PER_HANDLE);
            for _ in 0..3 {
                p.frame(&[top]);
                assert_eq!(p.cached.w.cache.templates(child), MAX_PER_HANDLE);
            }
            let few: Vec<Record> = [program(PROG)]
                .into_iter()
                .chain((2..N).flat_map(|i| [viewport(10.0 + i as f32), Record::Call(child)]))
                .collect();
            let top = p.put(&few);
            p.frame(&[top]);
            assert_eq!(p.frame(&[top]).1.walk_hits, MAX_PER_HANDLE as u32);
        });
    }

    #[test]
    fn a_handle_with_more_states_than_templates_turns_cold() {
        let n = MAX_PER_HANDLE * 6;
        for_both(|p| {
            let child = p.put(&[draw()]);
            let calls: Vec<Record> = [program(PROG)]
                .into_iter()
                .chain((0..n).flat_map(|i| [viewport(10.0 + i as f32), Record::Call(child)]))
                .collect();
            let top = p.put(&calls);
            p.frame(&[top]);
            assert_eq!(p.cached.w.cache.templates(child), 0);
            let (d, c) = p.frame(&[top]);
            assert_eq!((c.cold_walks as usize, c.walk_misses as usize, d.draws.len()), (n, n, n));
        });
    }

    #[test]
    fn a_chain_of_calls_deeper_than_the_limit_is_cut_the_same_way() {
        for_both(|p| {
            let mut next = p.put(&[draw()]);
            let mut chain = vec![next];
            for _ in 0..9 {
                next = p.put(&[draw(), Record::Call(next)]);
                chain.push(next);
            }
            let (first, last) = (chain[chain.len() - 1], chain[0]);
            let mid = chain[chain.len() - 4];
            let top = p.put(&[Record::Call(mid), Record::Call(first), Record::Call(mid), Record::Call(last)]);
            for _ in 0..3 {
                p.frame(&[top]);
            }
            let me = p.put(&[Record::Call(me_self(p))]);
            p.frame(&[me]);
        });
    }

    /// The handle that the next `put` returns, so that a recording can call itself.
    fn me_self(p: &Pair) -> u64 {
        p.cached.at
    }

    #[test]
    fn clears_copies_and_reports_come_from_the_template() {
        for_both(|p| {
            let region = [0, 0, 0, 4, 4, 1];
            let words: Vec<u32> = [0x2000, 0, 0x100, 0, 0].into_iter().chain(region).collect();
            let child = p.put(&[
                clear(),
                ext(ExtCode::ClearDepthStencil, 0, &[1f32.to_bits(), 1, 0, 0xff]),
                ext(ExtCode::Setting, Setting::CopyRowStride as u16, &[512, 0]),
                ext(ExtCode::CopyBufferToTexture, 0, &words),
                ext(ExtCode::ReportCounter, 0, &[0, 0x40, 0]),
                Record::Copy { src: 0, dst: 0 },
                ext(ExtCode::ClearBuffer, 0, &[0x100, 0, 4, 0, 0]),
            ]);
            let top = p.put(&[Record::Call(child), Record::Call(child)]);
            for _ in 0..3 {
                let (d, _) = p.frame(&[top]);
                assert_eq!((d.clears.len(), d.copies.len(), d.reports.len()), (4, 6, 2));
                assert_eq!(d.copies[0].row_stride, 512);
            }
        });
    }

    #[test]
    fn the_walker_and_the_decoder_count_the_same_entries_with_templates() {
        use nxkit_nvn::frame::Cmd;
        use nxkit_nvn::front::FrontEnd;
        use nxkit_nvn::space::LinearSpace;

        let mut f = FrontEnd::new(LinearSpace::new(HOST, GPU, SIZE).unwrap(), VecMem::new(HOST, SIZE as usize));
        let mut rig = Rig::new();
        let region = [0, 0, 0, 4, 4, 1];
        let words: Vec<u32> = [0x2000, 0, 0x100, 0, 0].into_iter().chain(region).collect();
        let child_recs = vec![
            program(PROG),
            draw(),
            clear(),
            draw(),
            dispatch(),
            ext(ExtCode::ClearBuffer, 0, &[0x100, 0, 4, 0, 0]),
            update(UBO_AT, 16, 0, 3),
            ext(ExtCode::CopyBufferToTexture, 0, &words),
            Record::Copy { src: 0, dst: 0 },
        ];
        let record = |f: &mut FrontEnd<LinearSpace, VecMem>, cb: usize, at: u64, recs: &[Record]| {
            f.add_command_memory(cb, at, 4096).unwrap();
            f.begin_recording(cb).unwrap();
            for r in recs {
                f.write(cb, *r).unwrap();
            }
            f.end_recording(cb).unwrap()
        };
        let child = record(&mut f, 0, CODE, &child_recs);
        rig.put_at(CODE, &child_recs);
        let top_recs = [Record::Call(child.0), draw(), Record::Call(child.0), Record::Call(child.0)];
        let top = record(&mut f, 1, CODE + 0x1000, &top_recs);
        rig.put_at(CODE + 0x1000, &top_recs);
        for round in 0..3 {
            f.begin_frame();
            f.submit(top).unwrap();
            let mut n = [0usize; 4];
            for c in &f.frame().cmds {
                match c {
                    Cmd::Draw { .. } => n[0] += 1,
                    Cmd::ClearColor { .. } | Cmd::ClearDepthStencil { .. } => n[1] += 1,
                    Cmd::Dispatch { .. } => n[2] += 1,
                    Cmd::Copy { .. } | Cmd::Downsample { .. } => n[3] += 1,
                    _ => {}
                }
            }
            let (d, c) = rig.frame(&[top.0]);
            assert_eq!((d.draws.len(), d.clears.len(), d.dispatches.len(), d.copies.len()), (n[0], n[1], n[2], n[3]));
            assert_eq!(n, [7, 3, 3, 12]);
            assert_eq!(c.walk_hits, if round == 0 { 2 } else { 3 });
            let ords = crate::draws::ordinals(f.frame());
            assert_eq!(ords.last().map(|o| (o.draw, o.copy)), Some((n[0] as u32, n[3] as u32 - 1)));
        }
    }

    /// A small deterministic generator.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self, n: u64) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (self.0 >> 33) % n
        }
    }

    /// One random record that sets or reads state, from the kinds that the walker follows.
    fn random_record(r: &mut Lcg, callees: &[u64]) -> Record {
        const OTHER: u64 = GPU + 0xa000;
        let buffer = UBO_AT + 0x40 * r.next(4);
        match r.next(26) {
            0 => program(if r.next(2) == 0 { PROG } else { OTHER }),
            1 => viewport(10.0 + r.next(3) as f32),
            2 => ubo(r.next(3) as u16, buffer, 64),
            3 => vertex_buffer(r.next(3) as u16, GPU + 0x2000 + 0x100 * r.next(2), 256),
            4 => attribute(r.next(4) as u32, 0x0a + r.next(2) as u32, r.next(3) as u32),
            5 => stream(r.next(3) as u32, 4 * (1 + r.next(3) as u32)),
            6 => blend(r.next(3) as u32, 1 + r.next(3) as u32),
            7 => stencil_ref(1 + r.next(3) as u32, r.next(3) as u32),
            8 => Record::BindTexture { slot: r.next(70) as u8, handle: r.next(3) },
            9 => deferred(r.next(40) as u8, 1 + r.next(3) as u8, SRC),
            10 => deferred(60 + r.next(8) as u8, 1 + r.next(3) as u8, SRC + 8 * r.next(3)),
            11 => update(buffer, 64, 4 * r.next(8) as u32, r.next(100) as u32),
            12 => dispatch(),
            13 => clear(),
            14 => ext(ExtCode::Setting, Setting::DepthClamp as u16, &[r.next(2) as u32]),
            15 => ext(ExtCode::Scissor, 0, &[0, 0, 5 + r.next(3) as u32, 6]),
            16 => ext(ExtCode::State, 1, &[r.next(256) as u32]),
            17 => ext(ExtCode::State, 5, &[r.next(4) as u32, 0]),
            18 => ext(ExtCode::Setting, Setting::BlendColor as u16, &[r.next(2) as u32, 0, 0, 0]),
            19 => ext(ExtCode::Setting, Setting::StencilMask as u16, &[3, r.next(3) as u32]),
            20 | 21 if !callees.is_empty() => Record::Call(callees[r.next(callees.len() as u64) as usize]),
            22 => ext(ExtCode::Setting, Setting::CopyRowStride as u16, &[64 * r.next(3) as u32, 0]),
            23 => ext(ExtCode::CopyBufferToTexture, 0, &[1, 0, 2, 0, 0, 0, 0, 0, 4, 4, 1]),
            _ => draw(),
        }
    }

    #[test]
    fn random_recordings_walk_the_same_with_and_without_templates() {
        let mut total = 0;
        for seed in 0..300u64 {
            let mut p = Pair::new(seed % 2 == 1);
            let mut r = Lcg(seed);
            for rig in [&mut p.cached, &mut p.plain] {
                rig.w.program_set(GPU + 0xa000, &keys(5, 6));
            }
            let mut handles: Vec<u64> = Vec::new();
            for _ in 0..4 {
                let new: Vec<u64> = (0..6)
                    .map(|_| {
                        let recs: Vec<Record> = (0..2 + r.next(10)).map(|_| random_record(&mut r, &handles)).collect();
                        p.put(&recs)
                    })
                    .collect();
                handles.extend(new);
            }
            let tops: Vec<u64> = (0..3)
                .map(|_| {
                    let recs: Vec<Record> = [program(PROG)]
                        .into_iter()
                        .chain((0..8).map(|_| random_record(&mut r, &handles)))
                        .collect();
                    p.put(&recs)
                })
                .collect();
            for _ in 0..25 {
                let values: Vec<u8> = (0..48).map(|_| r.next(4) as u8 * 3).collect();
                if r.next(3) == 0 {
                    p.write(SRC, &values);
                }
                if r.next(4) == 0 {
                    p.write(HOST + 0x1000 + 4 * r.next(16), &[r.next(200) as u8; 4]);
                }
                total += p.frame(&tops).1.walk_hits;
            }
        }
        assert!(total > 1000, "{total} hits");
    }

    /// A frame of 1500 models of about 20 records and 2 draws each, walked with and without templates. Run with
    /// `cargo test -p nxkit-gpu --release walk_speed -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement"]
    fn walk_speed() {
        let time = |cache_on: bool| {
            let mut rig = Rig::new();
            rig.mem = VecMem::new(HOST, 16 << 20);
            rig.w.verify = false;
            rig.w.cache_on = cache_on;
            let models: Vec<u64> = (0..1500)
                .map(|i| {
                    let mut m = vec![deferred(0, 3, SRC), viewport(50.0 + (i % 3) as f32)];
                    m.extend(model(UBO_AT + 0x40 * (i % 7), 2));
                    m.extend([blend(0, 2), stencil_ref(1, 3), ubo(1, UBO_AT, 64), draw()]);
                    rig.put(&m)
                })
                .collect();
            let mut top = vec![program(PROG)];
            top.extend(models.iter().map(|m| Record::Call(*m)));
            let top = rig.put(&top);
            let mut best = u128::MAX;
            for _ in 0..10 {
                let start = std::time::Instant::now();
                rig.frame(&[top]);
                best = best.min(start.elapsed().as_micros());
            }
            best
        };
        let (plain, cached) = (time(false), time(true));
        eprintln!("walk of 1500 models: {plain} us without templates, {cached} us with");
    }

    #[test]
    fn the_cache_can_be_cleared_by_memory_writes_from_another_thread() {
        let q = Arc::new(Invalidations::default());
        let q2 = q.clone();
        std::thread::spawn(move || q2.push(10, 20)).join().unwrap();
        let mut out = Vec::new();
        q.drain(&mut out);
        assert_eq!(out, [(10, 20)]);
        q.drain(&mut out);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn a_key_matches_states_with_the_same_needed_values() {
        let mut needs = DRAW_READS;
        needs.add(&Bits::range(0, 6));
        let mut a = State::default();
        a.texture[5] = 7;
        a.viewport[2] = 4.0;
        let key = Key::of(&needs, &a, 0);
        assert!(key.matches(&a, 0));
        assert!(key.matches(&a.clone(), 0));
        // A field that the key does not need can differ.
        let other = Needs::of(&[sc::VIEWPORT]);
        let narrow = Key::of(&other.0, &a, 0);
        let mut b = a.clone();
        b.blend_color[0] = 1.0;
        assert!(narrow.matches(&b, 0));
        assert!(!key.matches(&b, 0));
        b.blend_color[0] = 0.0;
        b.viewport[2] = 5.0;
        assert!(!narrow.matches(&b, 0));
        let mut b = a.clone();
        b.ubo[1][3] = (0x10, 4);
        assert!(!key.matches(&b, 0));
        assert!(!Key::of(&needs, &a, 1 << 5).matches(&a, 0), "the provenance of a texture is part of the key");
        assert!(Key::of(&needs, &a, 1 << 5).matches(&a, 1 << 5));
    }

    struct Needs(Bits);

    impl Needs {
        fn of(groups: &[usize]) -> Self {
            Self(groups.iter().fold(Bits::NONE, |mut b, g| {
                b.set(scalar_bit(*g));
                b
            }))
        }
    }
}
