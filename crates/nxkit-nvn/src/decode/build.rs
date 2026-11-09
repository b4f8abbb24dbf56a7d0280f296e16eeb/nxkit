//! The builder that turns the records of one recording into a summary.

use super::{
    BLOCK_CLEAR, BLOCK_COMPUTE, BLOCK_COPY, BLOCK_SYNC, BLOCK_TARGETS, Ctx, MAX_BINDINGS, Needs,
    Ref, Sets, Slots, State, Summary, TAG_SHIFT, bound, texture_at, texture_of, watch_addr,
};
use crate::frame::{Cmd, CmdStream, RtSet, Span};
use crate::mem::GuestMem;
use crate::record::{NO_HANDLE, Record, deferred_handle};
use crate::resource::TexId;

pub(super) struct Builder {
    pub(super) st: State,
    pub(super) needs: Needs,
    pub(super) sets: Sets,
    pub(super) stream: CmdStream,
    pub(super) refs: Vec<Ref>,
    pub(super) children: Vec<u64>,
    pub(super) blockers: u8,
    pub(super) draws: u32,
    pub(super) unresolved: u32,
    pub(super) tex_list: Option<Span>,
    pub(super) img_list: Option<Span>,
    pub(super) scratch: Vec<TexId>,
    /// The bit of each sampling slot of `st.tex` that holds a texture.
    pub(super) tex_live: u128,
    /// The bit of each image slot of `st.img` that holds a texture.
    pub(super) img_live: u128,
    /// True when the recording itself resolved something, so that `refs` has entries of its own.
    pub(super) own_refs: bool,
    /// Time of the records and of the splices, for `NXKIT_DIAG=decodeprof`.
    pub(super) ns_apply: u64,
    pub(super) ns_splice: u64,
    pub(super) records: u64,
}

/// The distinct textures of the slots that `live` marks, in slot order.
fn collect(live: u128, slots: &[Option<TexId>; MAX_BINDINGS], out: &mut Vec<TexId>) {
    out.clear();
    for (_, t) in bound(live, slots) {
        if !out.contains(&t) {
            out.push(t);
        }
    }
}

/// Sets or clears the bit of `slot` in `live` after the slot got `value`.
fn mark(live: &mut u128, slot: usize, value: Option<TexId>) {
    let bit = 1u128 << slot;
    *live = if value.is_some() {
        *live | bit
    } else {
        *live & !bit
    };
}

pub(super) fn delta_slots(mask: u128, slots: &[Option<TexId>; MAX_BINDINGS]) -> Slots {
    let mut out = Vec::with_capacity(mask.count_ones() as usize);
    let mut rest = mask;
    while rest != 0 {
        let i = rest.trailing_zeros() as usize;
        rest &= rest - 1;
        out.push((i as u8, slots.get(i).copied().flatten()));
    }
    out
}

/// True for a handle of texture id 0. NVN reserves the first texture descriptors for itself, so a game never registers
/// a texture there, and binding such a handle binds no texture: it is an unbind, not a handle that failed to resolve.
pub(super) fn null_handle(h: u64) -> bool {
    h & u64::from(crate::descriptors::MAX_TEXTURE_ID) == 0
}

impl Builder {
    /// Counts a reference that names no texture, and keeps it as an example of `decode.unresolved`.
    fn unresolved(&mut self, what: std::fmt::Arguments<'_>) {
        self.unresolved += 1;
        crate::diag::example(crate::diag::Reason::DecodeUnresolved, what);
    }

    fn addr<M: GuestMem>(&mut self, cx: &Ctx<M>, a: u64) -> Option<TexId> {
        if a == 0 {
            return None;
        }
        let t = texture_at(cx.table, a);
        if watch_addr().is_some_and(|w| w == a & ((1 << TAG_SHIFT) - 1)) {
            static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if crate::diag::first(&SHOWN, 30) {
                eprintln!("DIAG watch addr word {a:#x} -> {t:?}");
            }
        }
        self.refs.push(Ref::Addr(a, t));
        self.own_refs = true;
        t
    }

    fn handle<M: GuestMem>(&mut self, cx: &Ctx<M>, h: u64) -> Option<TexId> {
        if h == NO_HANDLE {
            return None;
        }
        let t = texture_of(cx.desc, h);
        if t.is_none() && !null_handle(h) {
            self.unresolved(format_args!("texture handle {h:#x}"));
        }
        self.refs.push(Ref::Handle(h, t));
        self.own_refs = true;
        t
    }

    fn need_draw_state(&mut self) {
        self.needs.draw |= !self.sets.draw;
        self.needs.targets |= !self.sets.targets;
        self.needs.tex |= !self.sets.tex;
    }

    fn reads(&mut self) -> Span {
        if let Some(s) = self.tex_list {
            return s;
        }
        collect(self.tex_live, &self.st.tex, &mut self.scratch);
        let s = self.stream.push_list(&self.scratch);
        self.tex_list = Some(s);
        s
    }

    fn writes(&mut self) -> Span {
        if let Some(s) = self.img_list {
            return s;
        }
        collect(self.img_live, &self.st.img, &mut self.scratch);
        let s = self.stream.push_list(&self.scratch);
        self.img_list = Some(s);
        s
    }

    pub(super) fn apply<M: GuestMem>(&mut self, cx: &Ctx<M>, rec: Record) {
        match rec {
            Record::SetTargets { color, depth } => {
                let mut set = RtSet::default();
                for (slot, a) in set.color.iter_mut().zip(color) {
                    *slot = self.addr(cx, a);
                }
                set.depth = self.addr(cx, depth);
                self.st.targets = set;
                self.sets.targets = true;
                self.blockers |= BLOCK_TARGETS;
                self.stream.cmds.push(Cmd::SetRenderTargets(set));
            }
            Record::SetScissor { full } => {
                self.st.scissor_full = full;
                self.sets.scissor = true;
            }
            Record::ClearColor { slot } => {
                self.needs.scissor |= !self.sets.scissor;
                self.needs.targets |= !self.sets.targets;
                self.blockers |= BLOCK_CLEAR;
                let full = self.st.scissor_full;
                self.stream.cmds.push(Cmd::ClearColor { slot, full });
            }
            Record::ClearDepthStencil => {
                self.needs.scissor |= !self.sets.scissor;
                self.needs.targets |= !self.sets.targets;
                self.blockers |= BLOCK_CLEAR;
                let full = self.st.scissor_full;
                self.stream.cmds.push(Cmd::ClearDepthStencil { full });
            }
            Record::Discard { color, depth } => {
                self.needs.targets |= !self.sets.targets;
                self.blockers |= BLOCK_CLEAR;
                self.stream.cmds.push(Cmd::Discard { color, depth });
            }
            Record::BindState(s) => {
                self.st.draw = s;
                self.sets.draw = true;
            }
            Record::BindTexture { slot, handle } => {
                let t = self.handle(cx, handle);
                self.st.tex[slot as usize] = t;
                mark(&mut self.tex_live, usize::from(slot), t);
                self.sets.tex |= 1 << slot;
                self.tex_list = None;
            }
            Record::BindTexturesDeferred {
                first,
                count,
                src,
                tex_base,
                samp_base,
            } => {
                for k in 0..count {
                    let slot = usize::from(first) + usize::from(k);
                    let Some(at) = self.st.tex.get(slot).map(|_| src + 8 * u64::from(k)) else {
                        break;
                    };
                    let (raw, handle) = match deferred_handle(cx.mem, src, k, tex_base, samp_base) {
                        Ok((_, raw, h)) => (raw, h),
                        Err(_) => {
                            self.unresolved(format_args!(
                                "deferred handle at {at:#x} is unreadable"
                            ));
                            (0, NO_HANDLE)
                        }
                    };
                    self.refs.push(Ref::Deferred(at, raw));
                    self.own_refs = true;
                    let t = self.handle(cx, handle);
                    self.st.tex[slot] = t;
                    mark(&mut self.tex_live, slot, t);
                    self.sets.tex |= 1 << slot;
                }
                self.tex_list = None;
            }
            Record::BindImage { slot, handle } => {
                let t = self.handle(cx, handle);
                self.st.img[slot as usize] = t;
                mark(&mut self.img_live, usize::from(slot), t);
                self.sets.img |= 1 << slot;
                self.img_list = None;
            }
            Record::Draw => {
                self.need_draw_state();
                let reads = self.reads();
                self.draws += 1;
                self.stream.cmds.push(Cmd::Draw {
                    state: self.st.draw,
                    reads,
                });
            }
            Record::Dispatch => {
                self.needs.tex |= !self.sets.tex;
                self.needs.img |= !self.sets.img;
                self.blockers |= BLOCK_COMPUTE;
                let (reads, writes) = (self.reads(), self.writes());
                self.stream.cmds.push(Cmd::Dispatch { reads, writes });
            }
            Record::Copy { src, dst } => {
                let (src, dst) = (self.addr(cx, src), self.addr(cx, dst));
                self.blockers |= BLOCK_COPY;
                self.stream.cmds.push(Cmd::Copy { src, dst });
            }
            Record::Downsample { src, dst } => {
                self.blockers |= BLOCK_COPY;
                match (self.addr(cx, src), self.addr(cx, dst)) {
                    (Some(src), Some(dst)) => self.stream.cmds.push(Cmd::Downsample { src, dst }),
                    // The walker of the encoder gives one detail entry for every downsample record. A copy
                    // without textures keeps the count of copy commands equal to that of the entries.
                    _ => {
                        self.unresolved(format_args!("downsample {src:#x} -> {dst:#x}"));
                        self.stream.cmds.push(Cmd::Copy {
                            src: None,
                            dst: None,
                        });
                    }
                }
            }
            Record::Barrier(bits) => {
                self.blockers |= BLOCK_SYNC;
                self.stream.cmds.push(Cmd::Barrier(bits));
            }
            Record::Sync => {
                self.blockers |= BLOCK_SYNC;
                self.stream.cmds.push(Cmd::Sync);
            }
            Record::Present(a) | Record::CpuRead(a) => {
                self.blockers |= BLOCK_SYNC;
                match self.addr(cx, a) {
                    Some(t) if matches!(rec, Record::Present(_)) => {
                        self.stream.cmds.push(Cmd::Present(t))
                    }
                    Some(t) => self.stream.cmds.push(Cmd::CpuRead(t)),
                    None => self.unresolved(format_args!("present or CPU read of {a:#x}")),
                }
            }
            Record::Ext(x) => self.apply_ext(cx, x),
            Record::End | Record::Jump(_) | Record::Call(_) => {}
        }
    }

    pub(super) fn splice(&mut self, handle: u64, c: &Summary) {
        let clock = crate::prof::Clock::start();
        self.splice_in(handle, c);
        self.ns_splice += clock.ns();
    }

    fn splice_in(&mut self, handle: u64, c: &Summary) {
        let (n, s) = (&c.needs, &self.sets);
        self.needs.targets |= n.targets && !s.targets;
        self.needs.scissor |= n.scissor && !s.scissor;
        self.needs.draw |= n.draw && !s.draw;
        self.needs.tex |= n.tex & !s.tex;
        self.needs.img |= n.img & !s.img;
        self.stream.splice(&c.stream);
        self.refs.extend_from_slice(&c.refs);
        self.children.push(handle);
        self.blockers |= c.blockers;
        self.draws += c.draws;
        self.unresolved += c.unresolved;
        let d = &c.delta;
        if let Some(t) = d.targets {
            self.st.targets = t;
            self.sets.targets = true;
        }
        if let Some(f) = d.scissor {
            self.st.scissor_full = f;
            self.sets.scissor = true;
        }
        if let Some(s) = d.draw {
            self.st.draw = s;
            self.sets.draw = true;
        }
        for (i, t) in &d.tex {
            self.st.tex[*i as usize] = *t;
            mark(&mut self.tex_live, usize::from(*i), *t);
            self.sets.tex |= 1 << i;
            self.tex_list = None;
        }
        for (i, t) in &d.img {
            self.st.img[*i as usize] = *t;
            mark(&mut self.img_live, usize::from(*i), *t);
            self.sets.img |= 1 << i;
            self.img_list = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handle_of_texture_id_zero_is_an_unbind() {
        assert!(null_handle(0));
        // The sampler id sits above the texture id; a sampler alone still binds no texture.
        assert!(null_handle(5 << 20));
        assert!(!null_handle(1));
        assert!(!null_handle(0x1930_0385));
    }
}
