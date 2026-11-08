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

use crate::error::Result;
use crate::passes::{LoadAction, PassKind, PassPlan, Slot, StoreAction};
use crate::resource::ResourceTable;

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
            self.live[i] = if local { Live::No } else { Live::Assumed };
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
                a.store = if a.discard_end || after == Live::No {
                    StoreAction::DontCare
                } else {
                    StoreAction::Store
                };
                a.store_assumed = a.store == StoreAction::Store && after == Live::Assumed;
                a.load = if a.clear_start {
                    LoadAction::Clear
                } else if a.discard_start {
                    LoadAction::DontCare
                } else if a.old_read || a.sampled {
                    LoadAction::Load
                } else if a.store == StoreAction::DontCare {
                    LoadAction::DontCare
                } else {
                    LoadAction::Load
                };
                a.memoryless = a.store == StoreAction::DontCare
                    && a.load != LoadAction::Load
                    && !a.sampled
                    && self.uses[i] == 1;
                match a.load {
                    LoadAction::Load => self.live[i] = Live::Proven,
                    _ if self.single[i] => self.live[i] = Live::No,
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
                if a.memoryless {
                    st.bytes_memoryless += a.bytes;
                }
                debug_assert!(a.slot == Slot::Depth || matches!(a.slot, Slot::Color(_)));
            }
            for t in &plan.reads[p.reads.clone()] {
                self.live[t.0 as usize] = Live::Proven;
            }
        }
        st.bytes_saved = st
            .bytes_naive
            .saturating_sub(st.bytes_loaded + st.bytes_stored);
        Ok(())
    }
}
