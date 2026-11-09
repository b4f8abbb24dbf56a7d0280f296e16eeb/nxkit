//! What the encode derives from a frame before it cuts passes: texture uses, declared views and vertex sampling.

use super::*;

/// How each texture of the frame is used. A texture is memoryless only when every attachment use says so and no
/// command samples, copies, presents or reads it.
pub(crate) fn collect_uses(frame: &Frame, plan: &PassPlan) -> FastMap<TexId, Use> {
    let mut other: FastSet<TexId> = FastSet::default();
    let mut read_back: FastSet<TexId> = FastSet::default();
    for c in &frame.cmds {
        match *c {
            Cmd::Draw { reads, .. } => other.extend(frame.list(reads).iter().copied()),
            Cmd::Dispatch { reads, writes } => {
                other.extend(frame.list(reads).iter().copied());
                other.extend(frame.list(writes).iter().copied());
            }
            Cmd::Copy { src, dst } => other.extend(src.into_iter().chain(dst)),
            Cmd::Downsample { src, dst } => other.extend([src, dst]),
            Cmd::Present(id) => {
                other.insert(id);
            }
            Cmd::CpuRead(id) => {
                other.insert(id);
                read_back.insert(id);
            }
            _ => {}
        }
    }
    let mut uses: FastMap<TexId, Use> = FastMap::default();
    for a in &plan.attachments {
        let u = uses.entry(a.tex).or_insert(Use {
            attachment: true,
            memoryless: true,
            cpu_read: false,
            atomic: false,
        });
        u.memoryless &= a.memoryless && !other.contains(&a.tex);
    }
    for id in other {
        uses.entry(id).or_default();
    }
    for id in read_back {
        uses.entry(id).or_default().cpu_read = true;
    }
    uses
}

/// The texture types that the shader of `key` declares, as (slot, type). The list is cached by key and borrowed.
pub(super) fn declared_views<'a>(
    decl: &'a mut [FastMap<String, Vec<(u32, nxkit_metal::TextureKind)>>; 2],
    programs: &crate::programs::Programs,
    key: &str,
    vertex: bool,
) -> &'a [(u32, nxkit_metal::TextureKind)] {
    let map = &mut decl[usize::from(vertex)];
    if !map.contains_key(key) {
        let v = programs
            .stage(key, vertex)
            .map(|p| {
                p.binds
                    .iter()
                    .filter(|b| b.kind == crate::programs::BindKind::Tex)
                    .filter_map(|b| b.dim.map(|k| (b.reg, k)))
                    .collect()
            })
            .unwrap_or_default();
        map.insert(key.to_owned(), v);
    }
    map.get(key).map_or(&[][..], Vec::as_slice)
}

/// For each pass, whether one of its draws binds a texture in the vertex stage. Only such a render pass has to hold its
/// vertex stage until an earlier pass wrote what it samples; the others let their vertex work overlap the fragment work
/// before them.
pub(super) fn vertex_sampling_passes(
    frame: &Frame,
    plan: &PassPlan,
    detail: &FrameDetail,
    ords: &[crate::draws::Ordinal],
) -> Vec<bool> {
    plan.passes
        .iter()
        .map(|p| {
            if p.kind != nxkit_nvn::passes::PassKind::Render {
                return false;
            }
            let first = ords.get(p.cmds.start as usize).map_or(0, |o| o.draw) as usize;
            let n = p
                .cmds
                .clone()
                .filter(|i| {
                    matches!(
                        frame.cmds.get(*i as usize),
                        Some(nxkit_nvn::frame::Cmd::Draw { .. })
                    )
                })
                .count();
            detail.draws.get(first..first + n).is_none_or(|draws| {
                draws.iter().any(|d| {
                    d.textures
                        .iter()
                        .any(|t| t.stage == crate::detail::Stage::Vertex)
                })
            })
        })
        .collect()
}
