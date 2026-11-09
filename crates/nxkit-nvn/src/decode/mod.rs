//! Reads a recording from command memory and gives its summary.
//!
//! A [`Summary`] is the effect stream of one recording: the [`Cmd`](crate::frame::Cmd) values that the pass analysis reads.
//! It also says which state the recording takes from its caller ([`Needs`]), which state it leaves behind ([`Delta`]),
//! and which addresses and handles it resolved ([`Ref`]). The cache in [`crate::cache`] keeps summaries per handle.
//!
//! A call record plays another recording. The child summary is made with the state that the caller has at that point.
//! The effects of the child go into the stream of the caller. State that the child sets stays set for the caller.

use std::ops::Range;

use crate::cache::RecCache;
use crate::descriptors::Descriptors;
use crate::error::{NvnError, Result};
use crate::frame::{CmdStream, DrawState, RtSet};
use crate::mem::GuestMem;
use crate::record::Record;
use crate::resource::{ResourceTable, TexId};
use crate::space::GpuAddr;

use build::{Builder, delta_slots};

mod build;
mod ext;

/// The number of texture and image slots.
pub const MAX_BINDINGS: usize = 128;
/// The deepest nesting of calls.
pub const MAX_CALL_DEPTH: u32 = 8;
const MAX_RECORDS: u32 = 1 << 20;

/// The recording changes attachments.
pub const BLOCK_TARGETS: u8 = 1;
/// The recording clears or discards.
pub const BLOCK_CLEAR: u8 = 2;
/// The recording dispatches compute.
pub const BLOCK_COMPUTE: u8 = 4;
/// The recording copies or resolves.
pub const BLOCK_COPY: u8 = 8;
/// The recording has a barrier, a fence, a present or a CPU read.
pub const BLOCK_SYNC: u8 = 16;

/// The state of a command buffer at one point of a recording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    /// The attachments.
    pub targets: RtSet,
    /// The scissor covers the whole target.
    pub scissor_full: bool,
    /// The state of the next draws.
    pub draw: DrawState,
    /// The textures by sampling slot.
    pub tex: [Option<TexId>; MAX_BINDINGS],
    /// The images by slot.
    pub img: [Option<TexId>; MAX_BINDINGS],
}

impl Default for State {
    /// The state of a queue when it starts a submitted recording.
    fn default() -> Self {
        Self {
            targets: RtSet::default(),
            scissor_full: true,
            draw: DrawState {
                color_write: 0xff,
                ..DrawState::default()
            },
            tex: [None; MAX_BINDINGS],
            img: [None; MAX_BINDINGS],
        }
    }
}

impl State {
    /// Applies the state that a recording left behind. A queue calls this after each submit, because the state
    /// that one submit leaves is the inherited state of the next.
    pub fn apply(&mut self, d: &Delta) {
        if let Some(t) = d.targets {
            self.targets = t;
        }
        if let Some(f) = d.scissor {
            self.scissor_full = f;
        }
        if let Some(s) = d.draw {
            self.draw = s;
        }
        for (i, t) in &d.tex {
            self.tex[*i as usize] = *t;
        }
        for (i, t) in &d.img {
            self.img[*i as usize] = *t;
        }
    }

    /// Removes a texture from every slot, for a texture that was finalised.
    pub fn forget(&mut self, id: TexId) {
        let gone = |t: &mut Option<TexId>| {
            if *t == Some(id) {
                *t = None;
            }
        };
        self.targets.color.iter_mut().for_each(gone);
        gone(&mut self.targets.depth);
        self.tex.iter_mut().for_each(gone);
        self.img.iter_mut().for_each(gone);
    }
}

/// The parts of the inherited state that a recording reads before it sets them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Needs {
    /// The attachments.
    pub targets: bool,
    /// The scissor.
    pub scissor: bool,
    /// The draw state.
    pub draw: bool,
    /// Bit n is set for sampling slot n.
    pub tex: u128,
    /// Bit n is set for image slot n.
    pub img: u128,
}

/// The parts of the state that a recording sets.
#[derive(Clone, Copy, Debug, Default)]
struct Sets {
    targets: bool,
    scissor: bool,
    draw: bool,
    tex: u128,
    img: u128,
}

type Slots = Vec<(u8, Option<TexId>)>;

/// The state that a recording leaves behind. Only what it set is in it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Delta {
    targets: Option<RtSet>,
    scissor: Option<bool>,
    draw: Option<DrawState>,
    tex: Slots,
    img: Slots,
}

/// A resolution that a summary depends on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ref {
    /// A GPU address and the texture that starts there.
    Addr(u64, Option<TexId>),
    /// A texture handle and the texture it names.
    Handle(u64, Option<TexId>),
    /// The host address of a handle that a deferred bind reads, and the raw value it held.
    Deferred(u64, u64),
}

/// The inherited state that a summary was made with. Only the needed parts are in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    targets: Option<RtSet>,
    scissor: Option<bool>,
    draw: Option<DrawState>,
    tex: Vec<(u8, TexId)>,
    img: Vec<(u8, TexId)>,
}

/// The bound slots of `slots` that `mask` selects, in slot order. It visits the set bits of the mask only.
struct Bound<'a> {
    mask: u128,
    slots: &'a [Option<TexId>; MAX_BINDINGS],
}

impl Iterator for Bound<'_> {
    type Item = (u8, TexId);

    fn next(&mut self) -> Option<Self::Item> {
        while self.mask != 0 {
            let i = self.mask.trailing_zeros() as usize;
            self.mask &= self.mask - 1;
            if let Some(t) = self.slots.get(i).copied().flatten() {
                return Some((i as u8, t));
            }
        }
        None
    }
}

fn bound(mask: u128, slots: &[Option<TexId>; MAX_BINDINGS]) -> Bound<'_> {
    Bound { mask, slots }
}

/// The bound slots of `slots` that `mask` selects, as a list with room for all of them.
fn bound_list(mask: u128, slots: &[Option<TexId>; MAX_BINDINGS]) -> Vec<(u8, TexId)> {
    let mut out = Vec::with_capacity(mask.count_ones() as usize);
    out.extend(bound(mask, slots));
    out
}

/// The bit of each slot that holds a texture.
fn live(slots: &[Option<TexId>; MAX_BINDINGS]) -> u128 {
    slots
        .iter()
        .enumerate()
        .fold(0, |bits, (i, t)| bits | u128::from(t.is_some()) << i)
}

impl Key {
    fn of(n: &Needs, s: &State) -> Self {
        Self {
            targets: n.targets.then_some(s.targets),
            scissor: n.scissor.then_some(s.scissor_full),
            draw: n.draw.then_some(s.draw),
            tex: bound_list(n.tex, &s.tex),
            img: bound_list(n.img, &s.img),
        }
    }

    /// A hash of the key that `n` and `s` give, made without building the key. Equal keys have equal hashes, so a
    /// lookup compares the hashes of many summaries before it compares one key in full.
    pub fn hash_of(n: &Needs, s: &State) -> u64 {
        use std::hash::{BuildHasher, Hash, Hasher};
        let mut h = foldhash::fast::FixedState::with_seed(0x6e786b).build_hasher();
        n.targets.then_some(s.targets).hash(&mut h);
        n.scissor.then_some(s.scissor_full).hash(&mut h);
        n.draw.then_some(s.draw).hash(&mut h);
        for t in bound(n.tex, &s.tex) {
            t.hash(&mut h);
        }
        0xffu8.hash(&mut h);
        for t in bound(n.img, &s.img) {
            t.hash(&mut h);
        }
        h.finish()
    }

    /// True when `s` has the same values for everything in `n`.
    pub fn matches(&self, n: &Needs, s: &State) -> bool {
        self.targets == n.targets.then_some(s.targets)
            && self.scissor == n.scissor.then_some(s.scissor_full)
            && self.draw == n.draw.then_some(s.draw)
            && self.tex.iter().copied().eq(bound(n.tex, &s.tex))
            && self.img.iter().copied().eq(bound(n.img, &s.img))
    }
}

/// The result of one recording under one inherited state.
#[derive(Debug)]
pub struct Summary {
    /// The effects, including those of the recordings that it calls.
    pub stream: CmdStream,
    /// The state that the recording takes from its caller.
    pub needs: Needs,
    pub(crate) key: Key,
    /// [`Key::hash_of`] of the key.
    pub(crate) key_hash: u64,
    pub(crate) delta: Delta,
    /// The resolutions that the summary depends on.
    pub refs: Vec<Ref>,
    /// The handles of the recordings that it calls.
    pub children: Vec<u64>,
    /// The memory that the records of this recording occupy, not those of its children.
    pub spans: Vec<Range<u64>>,
    /// The kinds of commands that keep it from becoming an indirect command buffer. Zero means it can.
    pub blockers: u8,
    /// The number of draws, including those of its children.
    pub draws: u32,
    /// The number of addresses and handles that named no texture.
    pub unresolved: u32,
    /// How often the cache gave this summary.
    pub hits: u32,
    /// The `(texture table, descriptor pools)` epochs at which `refs` last held. See [`RecCache::find`].
    pub(crate) checked: (u64, u64),
    /// The deferred binds among `refs`. They read guest memory, so a hit reads them again every time.
    pub(crate) deferred: Vec<Ref>,
}

/// What the decoder reads.
pub struct Ctx<'a, M: GuestMem> {
    /// The guest memory.
    pub mem: &'a M,
    /// The textures.
    pub table: &'a ResourceTable,
    /// The descriptor pools.
    pub desc: &'a Descriptors,
}

/// The GPU address of `NXKIT_WATCH_ADDR` (hex, `0x` optional): the decoder prints the texture that each record word at
/// that address resolves to. Diagnostics only.
fn watch_addr() -> Option<u64> {
    static ADDR: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    *ADDR.get_or_init(|| {
        let v = std::env::var("NXKIT_WATCH_ADDR").ok()?;
        u64::from_str_radix(v.trim().trim_start_matches("0x"), 16).ok()
    })
}

/// The bit where a record word keeps the texture id above the GPU address. Texture storage ends below 2^40.
pub(super) const TAG_SHIFT: u32 = 40;

/// The record word of a texture: its GPU address, and above it the texture id plus one. Textures can share storage
/// (BotW makes a 854x480 and a 1920x1080 texture at one address), so the address alone does not name one texture.
/// An id that does not fit leaves the word untagged.
pub fn tag_address(addr: u64, id: TexId) -> u64 {
    match u64::from(id.0).checked_add(1) {
        Some(tag) if tag < 1 << (64 - TAG_SHIFT) && addr < 1 << TAG_SHIFT => {
            addr | tag << TAG_SHIFT
        }
        _ => addr,
    }
}

/// The texture of a record word. A tagged word names its texture while that texture lives at the address. Otherwise
/// the newest texture at the address wins.
pub fn texture_at(table: &ResourceTable, word: u64) -> Option<TexId> {
    let addr = word & ((1 << TAG_SHIFT) - 1);
    let tag = word >> TAG_SHIFT;
    if tag != 0 {
        let id = TexId((tag - 1) as u32);
        if table.get(id).is_ok_and(|r| r.address.get() == addr) {
            return Some(id);
        }
    }
    table.at_address(GpuAddr::new(addr).ok()?).last()
}

/// The texture of a handle.
pub fn texture_of(desc: &Descriptors, handle: u64) -> Option<TexId> {
    desc.resolve(handle).ok().map(|(slot, _)| slot.texture)
}

impl Ref {
    /// True when the resolution is still the same.
    pub fn holds(&self, table: &ResourceTable, desc: &Descriptors, mem: &impl GuestMem) -> bool {
        match *self {
            Self::Addr(a, t) => texture_at(table, a) == t,
            Self::Handle(h, t) => texture_of(desc, h) == t,
            Self::Deferred(at, raw) => {
                let mut b = [0u8; 8];
                mem.read(at, &mut b).is_ok() && u64::from_le_bytes(b) == raw
            }
        }
    }
}

/// Gives the summary of a recording under an inherited state. The cache answers when it can.
/// The result is the index of the summary in the entries of the handle.
pub fn summary<M: GuestMem>(
    cache: &mut RecCache,
    cx: &Ctx<M>,
    handle: u64,
    inherited: &State,
    depth: u32,
) -> Result<usize> {
    let clock = crate::prof::Clock::start();
    let found = cache.find(handle, inherited, cx.table, cx.desc, cx.mem);
    let p = &mut cache.prof;
    p.ns_find += clock.ns();
    p.lookups += 1;
    p.depth_sum += u64::from(depth);
    p.depth_max = p.depth_max.max(depth);
    if let Some(i) = found {
        p.hits += 1;
        return Ok(i);
    }
    p.misses += 1;
    cache.stats.misses += 1;
    let clock = crate::prof::Clock::start();
    let mut s = decode(cache, cx, handle, inherited, depth)?;
    if depth == 0 {
        cache.prof.ns_miss += clock.ns();
    }
    s.checked = (cx.table.epoch(), cx.desc.epoch());
    cache.stats.unresolved += u64::from(s.unresolved);
    crate::diag::note_n(
        crate::diag::Reason::DecodeUnresolved,
        u64::from(s.unresolved),
    );
    Ok(cache.insert(handle, s))
}

fn decode<M: GuestMem>(
    cache: &mut RecCache,
    cx: &Ctx<M>,
    handle: u64,
    inherited: &State,
    depth: u32,
) -> Result<Summary> {
    if depth > MAX_CALL_DEPTH {
        return Err(NvnError::BadRecord("calls are nested too deep"));
    }
    let mut b = Builder {
        tex_live: live(&inherited.tex),
        img_live: live(&inherited.img),
        st: inherited.clone(),
        needs: Needs::default(),
        sets: Sets::default(),
        stream: CmdStream::new(),
        refs: Vec::new(),
        children: Vec::new(),
        blockers: 0,
        draws: 0,
        unresolved: 0,
        tex_list: None,
        img_list: None,
        scratch: Vec::new(),
        own_refs: false,
        ns_apply: 0,
        ns_splice: 0,
        records: 0,
    };
    let mut spans = Vec::new();
    let (mut pos, mut from) = (handle, handle);
    for _ in 0..MAX_RECORDS {
        let clock = crate::prof::Clock::start();
        b.records += 1;
        let (rec, size) = match Record::read(cx.mem, pos) {
            Ok(v) => v,
            Err(e) => {
                crate::diag::note(
                    crate::diag::Reason::DecodeBadRecord,
                    format_args!("recording {handle:#x} record at {pos:#x}: {e}"),
                );
                static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                if crate::diag::on("decode") && crate::diag::first(&SHOWN, 50) {
                    eprintln!(
                        "DIAG decode fail handle {handle:#x} from {from:#x} pos {pos:#x} spans {spans:x?}: {e}"
                    );
                }
                return Err(e);
            }
        };
        let next = pos + u64::from(size);
        match rec {
            Record::End => {
                spans.push(from..next);
                b.ns_apply += clock.ns();
                let (ns_apply, ns_splice, records) = (b.ns_apply, b.ns_splice, b.records);
                let clock = crate::prof::Clock::start();
                let s = finish(b, inherited, spans);
                let p = &mut cache.prof;
                (p.ns_apply, p.ns_splice, p.records) = (
                    p.ns_apply + ns_apply,
                    p.ns_splice + ns_splice,
                    p.records + records,
                );
                p.ns_finish += clock.ns();
                return Ok(s);
            }
            Record::Jump(to) => {
                spans.push(from..next);
                (pos, from) = (to, to);
                continue;
            }
            Record::Call(h) => {
                b.ns_apply += clock.ns();
                let i = summary(cache, cx, h, &b.st, depth + 1)?;
                if let Some(c) = cache.entry(h, i) {
                    b.splice(h, c);
                }
            }
            other => {
                b.apply(cx, other);
                b.ns_apply += clock.ns();
            }
        }
        pos = next;
    }
    Err(NvnError::BadRecord("recording does not end"))
}

/// Drops the repeated resolutions and keeps the first of each, in order. A binding that is set again and again
/// would otherwise be checked once per repeat on every cache hit.
fn dedup_refs(refs: &mut Vec<Ref>) {
    let mut seen = std::collections::HashSet::with_capacity_and_hasher(
        refs.len(),
        foldhash::fast::RandomState::default(),
    );
    refs.retain(|r| seen.insert(*r));
}

fn finish(mut b: Builder, inherited: &State, spans: Vec<Range<u64>>) -> Summary {
    // The references of one called summary are unique already. Only a mix of sources can repeat.
    if b.own_refs || b.children.len() > 1 {
        dedup_refs(&mut b.refs);
    }
    let deferred = b
        .refs
        .iter()
        .copied()
        .filter(|r| matches!(r, Ref::Deferred(..)))
        .collect();
    b.stream.seal();
    let (st, sets) = (&b.st, &b.sets);
    let delta = Delta {
        targets: sets.targets.then_some(st.targets),
        scissor: sets.scissor.then_some(st.scissor_full),
        draw: sets.draw.then_some(st.draw),
        tex: delta_slots(sets.tex, &st.tex),
        img: delta_slots(sets.img, &st.img),
    };
    Summary {
        key: Key::of(&b.needs, inherited),
        key_hash: Key::hash_of(&b.needs, inherited),
        needs: b.needs,
        delta,
        stream: b.stream,
        refs: b.refs,
        children: b.children,
        spans,
        blockers: b.blockers,
        draws: b.draws,
        unresolved: b.unresolved,
        hits: 0,
        checked: (u64::MAX, u64::MAX),
        deferred,
    }
}
