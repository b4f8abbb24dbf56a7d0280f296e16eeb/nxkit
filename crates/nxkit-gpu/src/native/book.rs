//! Booking the plan's writes with the frame.

use super::*;

/// Books the textures the plan writes as written in the pass of its anchor dispatch: the barrier planner, the
/// GPU-written list and the copy-back then see them whatever the frame's dispatch lists say.
pub(crate) fn book_writes(
    p: &Prepared,
    frame: &Frame,
    plan: &nxkit_nvn::passes::PassPlan,
    ords: &[crate::draws::Ordinal],
    images: &mut [crate::images::PassImages],
) {
    let Some(index) = anchor_pass(p, frame, plan, ords) else {
        return;
    };
    let Some(slot) = images.get_mut(index) else {
        return;
    };
    for id in p.writes() {
        if !slot.writes.contains(&id) {
            slot.writes.push(id);
        }
    }
}

/// The index of the pass that holds the anchor dispatch.
fn anchor_pass(
    p: &Prepared,
    frame: &Frame,
    plan: &nxkit_nvn::passes::PassPlan,
    ords: &[crate::draws::Ordinal],
) -> Option<usize> {
    plan.passes.iter().position(|pass| {
        pass.kind == nxkit_nvn::passes::PassKind::Compute
            && pass.cmds.clone().any(|i| {
                matches!(frame.cmds.get(i as usize), Some(Cmd::Dispatch { .. }))
                    && ords
                        .get(i as usize)
                        .is_some_and(|o| p.is_anchor(o.dispatch))
            })
    })
}

/// Whether a command after `anchor_cmd` reads each intermediate picture the plan writes.
pub(super) fn intermediates_read_later(
    frame: &Frame,
    anchor_cmd: usize,
    cx: &PrepareCx<'_>,
    textures: &[(GameResource, Texture, TextureDesc, TexId)],
) -> Vec<(GameResource, bool)> {
    let mut out = Vec::new();
    for (r, _, _, id) in textures {
        if !matches!(r, GameResource::Intermediate { .. }) {
            continue;
        }
        let backing = |t: &TexId| cx.textures.get(t).map(|res| res.barrier_id);
        let want = backing(id);
        let same = |t: &TexId| want.is_some() && backing(t) == want;
        let read = frame.cmds.iter().skip(anchor_cmd + 1).any(|c| match *c {
            Cmd::Draw { reads, .. } | Cmd::Dispatch { reads, .. } => {
                frame.list(reads).iter().any(same)
            }
            Cmd::Copy { src, .. } => src.iter().any(same),
            Cmd::Downsample { src, .. } => same(&src),
            Cmd::Present(t) | Cmd::CpuRead(t) => same(&t),
            _ => false,
        });
        out.push((*r, read));
    }
    out
}
