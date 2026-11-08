//! The render pass inference, first half: cut the frame into passes and merge them.
//!
//! The scan goes forward over the recorded commands and does five things.
//! - It cuts the stream when the attachments change, when a barrier comes, when other work (compute, copy, present)
//!   comes, and when a full clear hits an attachment that the pass already used.
//! - It notes for each attachment how the pass starts (a full clear, a discard, a read of the old contents).
//! - It does not cut for a copy without a texture on either side (a uniform update, a buffer copy or a buffer clear).
//!   The encoder does not run such a copy on the GPU, so it needs no pass of its own. The same rule lets compute
//!   dispatches on both sides of it share one pass.
//! - It merges a pass into the pass before it when both have the same attachments, only a barrier separates them,
//!   and the second pass does not sample an attachment of the first.
//! - It hands the result to a second step that picks the load and store actions.
//!
//! A clear under a scissor is a partial clear. It does not cut the pass. The old contents stay in use.

use crate::actions::Actions;
use crate::error::{NvnError, Result};
use crate::frame::{Cmd, DrawState, Frame, MAX_COLOR, RtSet, Span};
use crate::passes::{
    AttachmentPlan, Boundary, LoadAction, Pass, PassKind, PassPlan, Slot, StoreAction,
};
use crate::resource::{ResourceTable, TexId};

/// The analyser. It owns scratch memory that it reuses for every frame.
#[derive(Debug, Default)]
pub struct Analyser {
    actions: Actions,
    frames: u64,
}

struct Scan<'a> {
    frame: &'a Frame,
    table: &'a ResourceTable,
    plan: &'a mut PassPlan,
    rt: RtSet,
    open: bool,
    pending: Boundary,
    foreign: Option<PassKind>,
    /// The bits of the barriers that came since the last pass started.
    barrier: Option<u32>,
    /// The state and the reads of the draw just before, while no other command came between. A draw that repeats
    /// them changes nothing in the plan.
    last_draw: Option<(DrawState, Span)>,
}

/// True under `NXKIT_DIAG=stats` (or `all`).
fn stats_diag() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| crate::diag::on("stats"))
}

impl Analyser {
    /// A new analyser.
    pub fn new() -> Self {
        Self::default()
    }

    /// Analyses a frame and fills `plan`. The previous content of `plan` is dropped.
    pub fn analyse(
        &mut self,
        frame: &Frame,
        table: &ResourceTable,
        plan: &mut PassPlan,
    ) -> Result<()> {
        plan.clear();
        let mut s = Scan {
            frame,
            table,
            plan,
            rt: RtSet::default(),
            open: false,
            pending: Boundary::Start,
            foreign: None,
            barrier: None,
            last_draw: None,
        };
        for (i, cmd) in frame.cmds.iter().enumerate() {
            s.step(i as u32, *cmd)?;
        }
        s.close_render(frame.cmds.len() as u32);
        self.actions.assign(table, plan)?;
        self.frames += 1;
        if stats_diag() && self.frames % 30 == 1 {
            eprintln!("nxkit-nvn: pass frame {}: {}", self.frames, plan.stats.describe());
        }
        Ok(())
    }
}

fn slots(rt: &RtSet) -> impl Iterator<Item = (Slot, TexId)> + '_ {
    rt.color
        .iter()
        .enumerate()
        .filter_map(|(i, t)| t.map(|t| (Slot::Color(i as u8), t)))
        .chain(rt.depth.map(|t| (Slot::Depth, t)))
}

impl Scan<'_> {
    fn step(&mut self, i: u32, cmd: Cmd) -> Result<()> {
        if !matches!(cmd, Cmd::Draw { .. }) {
            self.last_draw = None;
        }
        match cmd {
            Cmd::SetRenderTargets(set) => {
                if set != self.rt {
                    self.close_render(i);
                    self.rt = set;
                    self.pending = Boundary::Targets;
                }
            }
            Cmd::ClearColor { slot, full } => self.clear(i, Slot::Color(slot), full)?,
            Cmd::ClearDepthStencil { full } => self.clear(i, Slot::Depth, full)?,
            Cmd::Discard { color, depth } => self.discard(i, color, depth)?,
            Cmd::Draw { state, reads } => self.draw(i, state, reads)?,
            Cmd::Barrier(bits) => {
                self.close_render(i);
                self.foreign = None;
                self.barrier = Some(self.barrier.unwrap_or(0) | bits);
                if let Boundary::Barrier(old) = self.pending {
                    self.pending = Boundary::Barrier(old | bits);
                }
            }
            Cmd::Dispatch { reads, writes } => {
                let (r, w) = (self.frame.list(reads), self.frame.list(writes));
                self.foreign_pass(i, PassKind::Compute, r, w);
            }
            Cmd::Copy { src: None, dst: None } => self.plan.stats.buffer_copies_absorbed += 1,
            Cmd::Copy { src, dst } => {
                let (r, w) = (src.as_slice(), dst.as_slice());
                self.foreign_pass(i, PassKind::Copy, r, w);
            }
            Cmd::Downsample { src, dst } => self.foreign_pass(i, PassKind::Copy, &[src], &[dst]),
            Cmd::Sync => self.foreign_pass(i, PassKind::Sync, &[], &[]),
            Cmd::Present(t) => self.foreign_pass(i, PassKind::Present, &[t], &[]),
            Cmd::CpuRead(t) => self.foreign_pass(i, PassKind::CpuRead, &[t], &[]),
        }
        Ok(())
    }

    fn foreign_pass(&mut self, i: u32, kind: PassKind, reads: &[TexId], writes: &[TexId]) {
        self.close_render(i);
        let p = &mut *self.plan;
        let grouped = self.foreign == Some(kind) && p.passes.last().is_some_and(|l| l.kind == kind);
        if !grouped {
            let (r, w) = (p.reads.len(), p.writes.len());
            let begin = match self.barrier.take() {
                Some(bits) => Boundary::Barrier(bits),
                None if p.passes.is_empty() => Boundary::Start,
                None => Boundary::Foreign,
            };
            let counts = match kind {
                PassKind::Compute => &mut p.stats.compute_begins,
                _ => &mut p.stats.other_begins,
            };
            counts.add(begin);
            p.passes.push(Pass {
                kind,
                begin,
                cmds: i..i,
                attachments: p.attachments.len()..p.attachments.len(),
                reads: r..r,
                writes: w..w,
            });
        }
        p.reads.extend_from_slice(reads);
        p.writes.extend_from_slice(writes);
        let n = p.passes.len() - 1;
        let last = &mut p.passes[n];
        last.cmds.end = i + 1;
        last.reads.end = p.reads.len();
        last.writes.end = p.writes.len();
        self.foreign = Some(kind);
        self.pending = Boundary::Foreign;
    }

    fn open_render(&mut self, i: u32) -> Result<()> {
        if self.open {
            return Ok(());
        }
        if self.rt.is_empty() {
            return Err(NvnError::NoTarget(0));
        }
        let p = &mut *self.plan;
        let start = p.attachments.len();
        for (slot, tex) in slots(&self.rt) {
            let r = self.table.get(tex)?;
            let view = self.rt.views[match slot { Slot::Color(n) => usize::from(n), Slot::Depth => MAX_COLOR }];
            let bytes = r.desc.level_bytes(view.level())?;
            p.attachments.push(AttachmentPlan {
                tex,
                slot,
                view,
                bytes,
                load: LoadAction::Load,
                store: StoreAction::Store,
                memoryless: false,
                store_assumed: false,
                clear_start: false,
                discard_start: false,
                old_read: false,
                sampled: false,
                discard_end: false,
                touched: false,
            });
        }
        p.passes.push(Pass {
            kind: PassKind::Render,
            begin: self.pending,
            cmds: i..i,
            attachments: start..p.attachments.len(),
            reads: p.reads.len()..p.reads.len(),
            writes: p.writes.len()..p.writes.len(),
        });
        p.stats.render_begins.add(self.pending);
        self.barrier = None;
        self.open = true;
        self.foreign = None;
        // a barrier with no bits is the neutral state while a pass runs
        self.pending = Boundary::Barrier(0);
        Ok(())
    }

    fn att(&mut self, slot: Slot) -> Option<&mut AttachmentPlan> {
        let range = self.plan.passes.last()?.attachments.clone();
        self.plan.attachments[range]
            .iter_mut()
            .find(|a| a.slot == slot)
    }

    fn clear(&mut self, i: u32, slot: Slot, full: bool) -> Result<()> {
        self.open_render(i)?;
        let missing = |s: Slot| {
            NvnError::NoTarget(if let Slot::Color(n) = s {
                n
            } else {
                MAX_COLOR as u8
            })
        };
        if full && self.att(slot).ok_or(missing(slot))?.touched {
            self.close_render(i);
            self.pending = Boundary::FullClear;
            self.open_render(i)?;
        }
        let a = self.att(slot).ok_or(missing(slot))?;
        if full {
            a.clear_start |= !a.touched;
        } else {
            a.old_read |= !a.clear_start;
        }
        a.touched = true;
        a.discard_end = false;
        Ok(())
    }

    fn discard(&mut self, i: u32, color: u8, depth: bool) -> Result<()> {
        if self.rt.is_empty() {
            return Ok(());
        }
        self.open_render(i)?;
        let wanted = (0..MAX_COLOR as u8)
            .filter(|n| color & (1 << n) != 0)
            .map(Slot::Color);
        for slot in wanted.chain(depth.then_some(Slot::Depth)) {
            if let Some(a) = self.att(slot) {
                if a.touched {
                    a.discard_end = true;
                } else {
                    a.discard_start = true;
                }
            }
        }
        Ok(())
    }

    fn draw(&mut self, i: u32, st: DrawState, reads: Span) -> Result<()> {
        if self.last_draw == Some((st, reads)) {
            return Ok(());
        }
        self.open_render(i)?;
        self.last_draw = Some((st, reads));
        let cur = self.plan.passes.len() - 1;
        let range = self.plan.passes[cur].attachments.clone();
        for a in &mut self.plan.attachments[range.clone()] {
            let (used, reads_old) = match a.slot {
                Slot::Color(n) => {
                    let w = st.color_write & (1 << n) != 0;
                    (w, w && st.blend)
                }
                Slot::Depth => (
                    st.depth_write || st.depth_test || st.stencil_test,
                    st.depth_test || st.stencil_test,
                ),
            };
            if used {
                a.old_read |= reads_old && !a.clear_start;
                a.touched = true;
                a.discard_end = false;
            }
        }
        let start = self.plan.passes[cur].reads.start;
        for &t in self.frame.list(reads) {
            if !self.plan.reads[start..].contains(&t) {
                self.plan.reads.push(t);
            }
            if let Some(a) = self.plan.attachments[range.clone()]
                .iter_mut()
                .find(|a| a.tex == t)
            {
                a.sampled = true;
            }
        }
        Ok(())
    }

    fn close_render(&mut self, end: u32) {
        if !self.open {
            return;
        }
        self.open = false;
        let p = &mut *self.plan;
        let n = p.passes.len() - 1;
        p.passes[n].cmds.end = end;
        p.passes[n].reads.end = p.reads.len();
        p.stats.render_passes_before_merge += 1;
        let atts = p.passes[n].attachments.clone();
        p.stats.bytes_naive += p.attachments[atts].iter().map(|a| 2 * a.bytes).sum::<u64>();
        if n > 0 && can_merge(p, n - 1, n) {
            merge(p, n - 1, n);
        }
    }
}

fn can_merge(p: &PassPlan, a: usize, b: usize) -> bool {
    let (pa, pb) = (&p.passes[a], &p.passes[b]);
    if pa.kind != PassKind::Render || !matches!(pb.begin, Boundary::Barrier(_)) {
        return false;
    }
    let (xa, xb) = (
        &p.attachments[pa.attachments.clone()],
        &p.attachments[pb.attachments.clone()],
    );
    if xa.len() != xb.len()
        || xa
            .iter()
            .zip(xb)
            .any(|(l, r)| l.tex != r.tex || l.slot != r.slot || l.view != r.view)
    {
        return false;
    }
    if xb.iter().any(|a| a.clear_start || a.discard_start) {
        return false;
    }
    !p.reads[pb.reads.clone()]
        .iter()
        .any(|t| xa.iter().any(|a| a.tex == *t))
}

fn merge(p: &mut PassPlan, a: usize, b: usize) {
    let (ra, rb) = (
        p.passes[a].attachments.clone(),
        p.passes[b].attachments.clone(),
    );
    for k in 0..ra.len() {
        let q = p.attachments[rb.start + k];
        let x = &mut p.attachments[ra.start + k];
        x.old_read |= q.old_read && !x.touched && !x.clear_start;
        x.sampled |= q.sampled;
        if q.touched {
            x.discard_end = q.discard_end;
        }
        x.touched |= q.touched;
    }
    p.attachments.truncate(rb.start);
    let q = p.passes.swap_remove(b);
    p.stats.render_begins.remove(q.begin);
    p.passes[a].cmds.end = q.cmds.end;
    p.passes[a].reads.end = q.reads.end;
}
