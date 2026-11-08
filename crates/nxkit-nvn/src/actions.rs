//! The render pass inference, second half: load actions, store actions and memoryless attachments.
//!
//! The walk goes backward over the passes. For each texture it knows whether anything later reads its contents.
//! - `Proven` means a later pass loads it, samples it, copies from it, presents it or the CPU reads it.
//! - `Assumed` means nothing in this frame reads it, but a later frame may. It is the state at the end of the frame.
//! - `No` means the contents are dead. A discard or a texture that the host marked frame-local gives this.
//!
//! The store action is `Store` unless the texture is dead after the pass.
//! The host does not guess that a texture is dead at the end of a frame, so a history buffer stays stored.
//! The bytes that are stored only because of `Assumed` are counted apart.
//!
//! One exception learns from the frames. The walk also knows, for each texture, whether the first use of the frame
//! reads the old contents. A texture whose first use was a clear or a discard in many frames in a row, and that no
//! frame read before overwriting it, is a scratch target: what it holds at the end of the frame is dead. Such a
//! texture starts the backward walk as `No` instead of `Assumed`. The evidence comes from a second liveness that
//! ignores the learned state, so a texture that the game starts to read cannot hide behind its own decision. A learned
//! store is never memoryless, since a wrong guess would then lose the texture and not only one frame.

use crate::error::Result;
use crate::passes::{LoadAction, PassKind, PassPlan, Slot, StoreAction};
use crate::resource::ResourceTable;

/// The frames in a row in which a texture must start dead before its end of frame counts as dead.
const LEARN_FRAMES: u16 = 120;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Live {
    No,
    Assumed,
    Proven,
}

/// Scratch memory of the walk. The vectors are indexed by texture id and persist. Only the entries of the textures
/// that the plan uses are valid, `touched` lists them so the next frame can reset them.
#[derive(Debug, Default)]
pub(crate) struct Actions {
    live: Vec<Live>,
    /// The liveness without what was learned: the evidence for the next frame.
    evidence: Vec<Live>,
    /// The frames in a row (saturating) in which the texture started dead, by evidence.
    streak: Vec<u16>,
    /// The texture starts this frame's walk dead because of its streak.
    learned: Vec<bool>,
    uses: Vec<u32>,
    single: Vec<bool>,
    touched: Vec<u32>,
}

impl Actions {
    /// Counts one use of the texture `i` and starts its entries on the first use of the frame.
    fn touch(&mut self, table: &ResourceTable, i: usize) {
        if self.uses[i] == 0 {
            let rec = table.get(crate::resource::TexId(i as u32)).ok();
            let local = rec.is_some_and(|r| r.frame_local);
            let learned = !local && crate::passes::loadstore_on() && self.streak[i] >= LEARN_FRAMES;
            self.learned[i] = learned;
            self.evidence[i] = if local { Live::No } else { Live::Assumed };
            self.live[i] = if local || learned {
                Live::No
            } else {
                Live::Assumed
            };
            self.single[i] = rec.is_some_and(|r| r.desc.levels <= 1 && r.desc.depth <= 1);
            self.touched.push(i as u32);
        }
        self.uses[i] += 1;
    }

    pub(crate) fn assign(&mut self, table: &ResourceTable, plan: &mut PassPlan) -> Result<()> {
        for i in self.touched.drain(..) {
            self.uses[i as usize] = 0;
        }
        let n = table.id_count();
        if self.uses.len() < n {
            self.live.resize(n, Live::Assumed);
            self.evidence.resize(n, Live::Assumed);
            self.streak.resize(n, 0);
            self.learned.resize(n, false);
            self.uses.resize(n, 0);
            self.single.resize(n, false);
        }
        for p in &plan.passes {
            for a in &plan.attachments[p.attachments.clone()] {
                self.touch(table, a.tex.0 as usize);
            }
            for t in plan.reads[p.reads.clone()]
                .iter()
                .chain(&plan.writes[p.writes.clone()])
            {
                self.touch(table, t.0 as usize);
            }
        }
        let st = &mut plan.stats;
        st.passes = plan.passes.len() as u32;
        st.render_passes = plan
            .passes
            .iter()
            .filter(|p| p.kind == PassKind::Render)
            .count() as u32;
        for p in plan.passes.iter().rev() {
            for a in &mut plan.attachments[p.attachments.clone()] {
                let i = a.tex.0 as usize;
                let after = self.live[i];
                let after_ev = self.evidence[i];
                a.store = if a.discard_end || after == Live::No {
                    StoreAction::DontCare
                } else {
                    StoreAction::Store
                };
                a.store_assumed = a.store == StoreAction::Store && after == Live::Assumed;
                // What the store would be without the learned state.
                let store_ev_dead = a.discard_end || after_ev == Live::No;
                let learned_store = a.store == StoreAction::DontCare && !store_ev_dead;
                a.load = pick_load(a, a.store == StoreAction::DontCare);
                let load_ev = pick_load(a, store_ev_dead);
                a.memoryless = a.store == StoreAction::DontCare
                    && !learned_store
                    && a.load != LoadAction::Load
                    && !a.sampled
                    && self.uses[i] == 1;
                match a.load {
                    LoadAction::Load => self.live[i] = Live::Proven,
                    _ if self.single[i] => self.live[i] = Live::No,
                    _ => {}
                }
                match load_ev {
                    LoadAction::Load => self.evidence[i] = Live::Proven,
                    _ if self.single[i] => self.evidence[i] = Live::No,
                    _ => {}
                }
                if a.load == LoadAction::Load {
                    st.bytes_loaded += a.bytes;
                }
                if a.store == StoreAction::Store {
                    st.bytes_stored += a.bytes;
                    if a.store_assumed {
                        st.bytes_stored_assumed += a.bytes;
                    }
                }
                if learned_store {
                    st.stores_learned_dead += 1;
                    st.bytes_learned_dead += a.bytes;
                }
                if a.memoryless {
                    st.bytes_memoryless += a.bytes;
                }
                debug_assert!(a.slot == Slot::Depth || matches!(a.slot, Slot::Color(_)));
            }
            for t in &plan.reads[p.reads.clone()] {
                self.live[t.0 as usize] = Live::Proven;
                self.evidence[t.0 as usize] = Live::Proven;
            }
        }
        // The walk ended at the start of the frame. A texture that starts dead by evidence adds to its streak, one
        // that starts read (Proven) loses it, and one that is only assumed keeps it.
        for &i in &self.touched {
            let i = i as usize;
            match self.evidence[i] {
                Live::No => self.streak[i] = self.streak[i].saturating_add(1),
                Live::Proven => self.streak[i] = 0,
                Live::Assumed => {}
            }
        }
        st.bytes_saved = st
            .bytes_naive
            .saturating_sub(st.bytes_loaded + st.bytes_stored);
        Ok(())
    }
}

/// The load action of an attachment, given whether its store is dead.
fn pick_load(a: &crate::passes::AttachmentPlan, store_dead: bool) -> LoadAction {
    if a.clear_start {
        LoadAction::Clear
    } else if a.discard_start {
        LoadAction::DontCare
    } else if a.old_read || a.sampled {
        LoadAction::Load
    } else if store_dead {
        LoadAction::DontCare
    } else {
        LoadAction::Load
    }
}
