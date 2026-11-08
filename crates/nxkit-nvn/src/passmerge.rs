//! The `NXKIT_DIAG=passmerge` report: what the pass plan of a frame still leaves on the table.
//!
//! The report looks at the finished plan, after the merge of [`crate::analyse`]. It counts
//! - render passes with no draw and no clear (zero when the optimiser is on, it drops them),
//! - neighbouring render passes with equal attachments that stayed apart, and why,
//! - two such passes with one compute or copy pass between them, and whether that pass touches their textures
//!   (texture lists only: a buffer that the dispatch writes and a draw reads is not known),
//! - loads that are `Load` only because a draw might cover a part of the target, and attachments that no draw wrote,
//! - stores that are kept only because a later frame might read the texture.
//!
//! It changes nothing. The host adds the draws that cover the whole target (it knows the geometry).

use crate::frame::{Cmd, Frame};
use crate::passes::{AttachmentPlan, Pass, PassKind, PassPlan};
use crate::resource::TexId;

/// The counters of one frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MergeReport {
    /// Render passes in the plan.
    pub render: u32,
    /// Render passes with exactly one draw.
    pub one_draw: u32,
    /// Render passes with no draw and no clear.
    pub empty: u32,
    /// Neighbouring render passes with equal attachments, not merged: the second starts with a clear or a discard.
    pub blocked_clear: u32,
    /// Same, the second samples an attachment of the first.
    pub blocked_sampled: u32,
    /// Same, for another reason (the way the second begins).
    pub blocked_other: u32,
    /// Two render passes with equal attachments and one compute or copy pass between them that could merge otherwise.
    pub split_by_foreign: u32,
    /// Part of `split_by_foreign`: the pass between reads or writes none of the textures of the two. It could run
    /// before them if it touches no buffer that they use.
    pub split_independent: u32,
    /// Attachments that are loaded only by the default rule.
    pub load_default: u32,
    /// The bytes of `load_default`.
    pub load_default_bytes: u64,
    /// Attachments that no draw or clear of their pass wrote (loaded and stored for nothing).
    pub untouched: u32,
    /// The bytes of `untouched`.
    pub untouched_bytes: u64,
    /// Stores kept only because a later frame might read the texture.
    pub store_assumed: u32,
    /// The bytes of `store_assumed`.
    pub store_assumed_bytes: u64,
}

fn atts<'a>(p: &'a PassPlan, pass: &Pass) -> &'a [AttachmentPlan] {
    &p.attachments[pass.attachments.clone()]
}

fn same_attachments(a: &[AttachmentPlan], b: &[AttachmentPlan]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(l, r)| l.tex == r.tex && l.slot == r.slot && l.view == r.view)
}

fn writes_of<'a>(p: &'a PassPlan, pass: &Pass) -> &'a [TexId] {
    &p.writes[pass.writes.clone()]
}

/// Why two neighbouring render passes `a` and `b` with equal attachments do not merge, as an index into the
/// `blocked_*` counters (0 clear, 1 sampled, 2 other), or `None` when nothing but the neighbourhood is in the way.
fn blocked(p: &PassPlan, a: &Pass, b: &Pass) -> Option<usize> {
    let (xa, xb) = (atts(p, a), atts(p, b));
    if xb.iter().any(|x| x.clear_start || x.discard_start) {
        return Some(0);
    }
    if p.reads_of(b).iter().any(|t| xa.iter().any(|x| x.tex == *t)) {
        return Some(1);
    }
    if matches!(b.begin, crate::passes::Boundary::Barrier(_) | crate::passes::Boundary::Targets) {
        None
    } else {
        Some(2)
    }
}

impl MergeReport {
    /// Counts the plan of `frame`.
    pub fn of(frame: &Frame, plan: &PassPlan) -> Self {
        let mut r = Self::default();
        for (i, pass) in plan.passes.iter().enumerate() {
            if pass.kind != PassKind::Render {
                continue;
            }
            r.render += 1;
            let cmds = frame.cmds.get(pass.cmds.start as usize..pass.cmds.end as usize).unwrap_or(&[]);
            let draws = cmds.iter().filter(|c| matches!(c, Cmd::Draw { .. })).count();
            let clears = cmds.iter().any(|c| matches!(c, Cmd::ClearColor { .. } | Cmd::ClearDepthStencil { .. }));
            r.one_draw += u32::from(draws == 1);
            r.empty += u32::from(draws == 0 && !clears);
            for a in atts(plan, pass) {
                if a.load_is_default() {
                    r.load_default += 1;
                    r.load_default_bytes += a.bytes;
                }
                if a.is_untouched() {
                    r.untouched += 1;
                    r.untouched_bytes += a.bytes;
                }
                if a.store_assumed {
                    r.store_assumed += 1;
                    r.store_assumed_bytes += a.bytes;
                }
            }
            let Some(next) = plan.passes.get(i + 1) else { continue };
            let (xa, a_reads) = (atts(plan, pass), plan.reads_of(pass));
            if next.kind == PassKind::Render && same_attachments(xa, atts(plan, next)) {
                match blocked(plan, pass, next) {
                    Some(0) => r.blocked_clear += 1,
                    Some(1) => r.blocked_sampled += 1,
                    _ => r.blocked_other += 1,
                }
            }
            let Some(after) = plan.passes.get(i + 2) else { continue };
            let between = matches!(next.kind, PassKind::Compute | PassKind::Copy);
            if between && after.kind == PassKind::Render && same_attachments(xa, atts(plan, after)) && blocked(plan, pass, after).is_none() {
                r.split_by_foreign += 1;
                let (reads, writes) = (plan.reads_of(next), writes_of(plan, next));
                let in_a = |t: &TexId| xa.iter().any(|x| x.tex == *t);
                let touches_after = |t: &TexId| atts(plan, after).iter().any(|x| x.tex == *t);
                if !reads.iter().any(in_a) && !writes.iter().any(|t| in_a(t) || a_reads.contains(t) || touches_after(t)) {
                    r.split_independent += 1;
                }
            }
        }
        r
    }

    /// One line for the log.
    pub fn describe(&self) -> String {
        let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
        format!(
            "render {} (one draw {}, empty {}); unmerged equal neighbours: clear {} sampled {} other {}; \
             split by compute/copy {} (independent by texture {}); default loads {} ({:.1} MB), untouched attachments {} \
             ({:.1} MB); assumed stores {} ({:.1} MB)",
            self.render,
            self.one_draw,
            self.empty,
            self.blocked_clear,
            self.blocked_sampled,
            self.blocked_other,
            self.split_by_foreign,
            self.split_independent,
            self.load_default,
            mb(self.load_default_bytes),
            self.untouched,
            mb(self.untouched_bytes),
            self.store_assumed,
            mb(self.store_assumed_bytes),
        )
    }
}
