//! What the encode derives from a frame before it cuts passes: texture uses, declared views and vertex sampling.

use super::*;

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
/// vertex stage until an earlier pass wrote what it samples. The others let their vertex work overlap the fragment work
/// before them.
pub(super) fn vertex_sampling_passes(
    frame: &Frame,
    plan: &PassPlan,
    detail: &FrameDetail,
    ords: &[crate::ordinal::Ordinal],
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
