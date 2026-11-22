//! `NXKIT_BARPASS=<pass index>`: why the planner gave one pass the barrier it got.
//!
//! The trace helps when a pass shows stale data and the barrier before it is a guess. For the pass, every 60th frame
//! prints the kind, the commands, the planned barrier, each texture that the pass reads or writes with the stages that
//! the planner still holds for it, and every earlier pass of the frame that writes a texture that this pass reads.

use nxkit_metal::Dependency;
use nxkit_nvn::frame::Frame;
use nxkit_nvn::passes::{Pass, PassPlan};
use nxkit_nvn::resource::TexId;

use super::{BarrierState, Touch, touch};
use crate::fastmap::FastMap;

/// The frames between two lines of the trace.
const EVERY: u32 = 60;

impl BarrierState {
    /// Whether `NXKIT_BARPASS` names pass `index`.
    pub(super) fn traced(index: usize) -> bool {
        crate::settings::live().bar_pass == Some(index)
    }

    /// The texts of the textures `ids`: the id, the texture that it aliases, the pending write and read stages and the
    /// last pass that touched it.
    fn show(&self, ids: &[TexId], aliases: &FastMap<TexId, TexId>) -> String {
        ids.iter()
            .map(|id| {
                let r = aliases.get(id).unwrap_or(id);
                let h = self.pending.get(r);
                let alias = if r == id {
                    String::new()
                } else {
                    format!("->{r:?}")
                };
                format!(
                    "{id:?}{alias} write={:?} read={:?} last={:?}",
                    h.and_then(|h| h.write),
                    h.and_then(|h| h.read),
                    h.and_then(|h| h.last)
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// Prints the trace of pass `index` (`p`, which touches `t`) with the barrier `dep` that it gets (after `allbar` and the
    /// `nobar` file). Call it before the planner records the pass.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn trace_pass(
        &self,
        frame: &Frame,
        plan: &PassPlan,
        aliases: &FastMap<TexId, TexId>,
        images: &[crate::images::PassImages],
        index: usize,
        p: &Pass,
        t: &Touch,
        dep: Option<&Dependency>,
    ) {
        // Process-wide (W26): a rate limit of a diagnostic has no context to live in.
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        if !nxkit_nvn::diag::every(&N, EVERY) {
            return;
        }
        eprintln!(
            "DIAG barpass {index} kind {:?} cmds {:?} dependency {dep:?}\n  reads: {}\n  writes: {}",
            p.kind,
            p.cmds,
            self.show(&t.reads, aliases),
            self.show(&t.writes, aliases)
        );
        for (j, q) in plan.passes.iter().enumerate().take(index) {
            let mut written = touch(frame, plan, q).writes;
            if let Some(x) = images.get(j) {
                written.extend_from_slice(&x.writes);
            }
            let hit: Vec<TexId> = written
                .into_iter()
                .filter(|w| t.reads.contains(w))
                .collect();
            if !hit.is_empty() {
                eprintln!("  earlier pass {j} {:?} writes {hit:?}", q.kind);
            }
        }
    }
}
