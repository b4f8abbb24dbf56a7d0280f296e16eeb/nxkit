//! The hook points of the patch modules (W38, `nxkit-patch-abi`, `docs/diagnostics.md`).
//!
//! The encode path asks [`pass_on`] and [`draw_on`] first. Each is one relaxed atomic load, so a run without a patch
//! builds nothing and changes nothing. With a patch the encoder builds the read-only info structs here, calls the
//! patches through `nxkit_patch_abi::registry` and applies the merged answer. The apply functions are pure and have
//! unit tests. Only what the answers name changes: skip a pass or a draw, a full barrier before a pass, the load
//! action of colour slots, the focus mark of a pass, the blend state of a slot, the texture of a slot and the stencil
//! reference.

use std::collections::HashSet;
use std::ops::Range;

use nxkit_metal::{Dependency, LoadAction as MLoad, RenderPassDesc, Scope, Stages, Visibility};
use nxkit_nvn::passes::{Pass, Slot};
use nxkit_nvn::resource::TexId;
use nxkit_patch_abi as abi;
use nxkit_patch_abi::registry::{self, HOOK_DRAW, HOOK_FRAME, HOOK_PASS};

use crate::detail::{BlendDetail, DrawDetail, Stage};
use crate::drawctx::Shared;
use crate::stats::EncodeStats;

/// The barrier of the answer flag `PASS_BARRIER`: the pass waits for all earlier work of the frame.
pub(crate) fn full_barrier() -> Dependency {
    Dependency {
        scope: Scope::FromEarlierPasses,
        after: Stages::ALL,
        before: Stages::VERTEX | Stages::FRAGMENT | Stages::OBJECT | Stages::MESH,
        visibility: Visibility::Device,
    }
}

/// True when a patch has a pass hook.
pub(crate) fn pass_on() -> bool {
    registry::active(HOOK_PASS)
}

/// True when a patch has a draw hook. The encoder then does not replay recorded runs of draws, as with a skip list.
pub(crate) fn draw_on() -> bool {
    registry::active(HOOK_DRAW)
}

/// True when any patch is loaded: the handles of the frame are noted, so a draw can name its textures by id.
pub(crate) fn handles_on() -> bool {
    registry::active(HOOK_DRAW | HOOK_PASS)
}

/// Before a frame is encoded.
pub(crate) fn frame_begin(passes: usize, draws: usize) {
    if registry::active(HOOK_FRAME) {
        registry::frame_begin(&abi::FrameBegin {
            frame: crate::diag::frame(),
            passes: u32::try_from(passes).unwrap_or(u32::MAX),
            draws: u32::try_from(draws).unwrap_or(u32::MAX),
        });
    }
}

/// After a frame is encoded and committed.
pub(crate) fn frame_end(stats: &EncodeStats) {
    if registry::active(HOOK_FRAME) {
        registry::frame_end(&abi::FrameEnd {
            frame: crate::diag::frame(),
            render_passes: stats.render_passes,
            draws: stats.draws,
            skipped: stats.skipped_total(),
            command_buffers: stats.command_buffers,
            pipeline_builds: stats.pipeline_builds,
            encode_ns: stats.encode_ns,
        });
    }
}

fn load_code(l: MLoad) -> u32 {
    match l {
        MLoad::Load => 0,
        MLoad::Clear => 1,
        MLoad::DontCare => 2,
    }
}

/// The info of a render pass: the attachments of the Metal pass `desc` and the program pairs of its draws.
pub(crate) fn pass_info(
    sh: &Shared<'_>,
    pass: &Pass,
    index: usize,
    draws: &Range<u32>,
    desc: &RenderPassDesc,
) -> abi::PassInfo {
    let mut info = abi::PassInfo {
        frame: crate::diag::frame(),
        index: u32::try_from(index).unwrap_or(u32::MAX),
        first_draw: draws.start,
        draw_count: draws.len() as u32,
        ..Default::default()
    };
    let plan = sh.plan.attachments_of(pass);
    let tex_of = |slot: Slot| {
        plan.iter()
            .find(|a| a.slot == slot)
            .map_or(abi::TEX_UNKNOWN, |a| a.tex.0)
    };
    let mut n = 0;
    for (i, c) in desc.colors.iter().enumerate() {
        let (Some(c), true) = (c, n < abi::MAX_ATTACH && i < abi::MAX_COLORS) else {
            continue;
        };
        let d = c.texture.desc();
        info.attach[n] = abi::AttachInfo {
            tex_id: tex_of(Slot::Color(i as u8)),
            slot: i as u32,
            width: d.width,
            height: d.height,
            load: load_code(c.load),
            store: u32::from(matches!(c.store, nxkit_metal::StoreAction::DontCare)),
            format: abi::Key::new(&format!("{:?}", d.format)),
        };
        n += 1;
    }
    if let Some(d) = desc.depth.as_ref().filter(|_| n < abi::MAX_ATTACH) {
        let t = d.texture.desc();
        info.attach[n] = abi::AttachInfo {
            tex_id: tex_of(Slot::Depth),
            slot: abi::SLOT_DEPTH,
            width: t.width,
            height: t.height,
            load: load_code(d.load),
            store: u32::from(matches!(d.store, nxkit_metal::StoreAction::DontCare)),
            format: abi::Key::new(&format!("{:?}", t.format)),
        };
        n += 1;
    }
    info.attach_count = n as u32;
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    for d in draws
        .clone()
        .filter_map(|n| sh.detail.draws.get(n as usize))
    {
        let pair = (
            &*d.program.vertex,
            d.program.fragment.as_deref().unwrap_or(""),
        );
        if !seen.insert(pair) {
            continue;
        }
        if let Some(slot) = info.programs.get_mut(info.program_count as usize) {
            *slot = abi::ProgramKeys {
                vertex: abi::Key::new(pair.0),
                fragment: abi::Key::new(pair.1),
            };
            info.program_count += 1;
        }
    }
    info.programs_total = seen.len() as u32;
    info
}

/// What a patch answered to a render pass.
pub(crate) fn pass(
    sh: &Shared<'_>,
    pass: &Pass,
    index: usize,
    draws: &Range<u32>,
    desc: &RenderPassDesc,
) -> Option<abi::PassAnswer> {
    if !pass_on() {
        return None;
    }
    Some(registry::pass(&pass_info(sh, pass, index, draws, desc)))
}

/// The load action and the clear colour that the answer gives colour slot `slot`, if it changes it.
/// A memoryless attachment keeps `DontCare` when the answer asks to load, since a memoryless texture has no content.
pub(crate) fn new_load(
    a: &abi::PassAnswer,
    slot: usize,
    memoryless: bool,
) -> Option<(MLoad, [f64; 4])> {
    if a.flags & abi::PASS_LOAD == 0 || slot >= 32 || a.load_slots & (1 << slot) == 0 {
        return None;
    }
    let clear = a.clear.map(f64::from);
    match a.load {
        abi::LOAD_KEEP if memoryless => Some((MLoad::DontCare, clear)),
        abi::LOAD_KEEP => Some((MLoad::Load, clear)),
        abi::LOAD_CLEAR => Some((MLoad::Clear, clear)),
        abi::LOAD_DONT_CARE => Some((MLoad::DontCare, clear)),
        _ => None,
    }
}

/// Applies the load answer to the colour attachments of the Metal pass.
pub(crate) fn apply_load(desc: &mut RenderPassDesc, a: &abi::PassAnswer) {
    for (i, c) in desc.colors.iter_mut().enumerate() {
        if let Some(c) = c
            && let Some((load, clear)) = new_load(a, i, c.memoryless)
        {
            c.load = load;
            c.clear = clear;
        }
    }
}

fn stage_code(s: Stage) -> u32 {
    match s {
        Stage::Vertex => 0,
        Stage::Fragment => 1,
        Stage::Compute => 2,
        Stage::Task => 3,
        Stage::Mesh => 4,
    }
}

/// The info of a draw. `tex_of` says which texture a handle resolved to.
pub(crate) fn draw_info(
    frame: u64,
    pass: usize,
    n: u32,
    d: &DrawDetail,
    tex_of: &dyn Fn(u64) -> Option<TexId>,
) -> abi::DrawInfo {
    let mut info = abi::DrawInfo {
        frame,
        pass: u32::try_from(pass).unwrap_or(u32::MAX),
        draw: n,
        topology: d.topology,
        programs: abi::ProgramKeys {
            vertex: abi::Key::new(&d.program.vertex),
            fragment: abi::Key::new(d.program.fragment.as_deref().unwrap_or("")),
        },
        write_masks: d.write_masks,
        depth_test: u32::from(d.depth_stencil.depth_test),
        depth_write: u32::from(d.depth_stencil.depth_write),
        stencil_test: u32::from(d.depth_stencil.stencil_test),
        stencil_ref: d.dynamic.stencil_ref,
        ..Default::default()
    };
    for (slot, b) in info.blend.iter_mut().zip(&d.blend) {
        *slot = blend_to_abi(b);
    }
    for (slot, t) in info.textures.iter_mut().zip(&d.textures) {
        *slot = abi::TexBind {
            stage: stage_code(t.stage),
            slot: t.slot,
            tex_id: tex_of(t.handle).map_or(abi::TEX_UNKNOWN, |id| id.0),
            handle: t.handle,
        };
        info.texture_count += 1;
    }
    info
}

fn blend_to_abi(b: &BlendDetail) -> abi::BlendState {
    abi::BlendState {
        enable: u32::from(b.enable),
        src_rgb: b.src_rgb,
        dst_rgb: b.dst_rgb,
        op_rgb: b.op_rgb,
        src_alpha: b.src_alpha,
        dst_alpha: b.dst_alpha,
        op_alpha: b.op_alpha,
    }
}

fn blend_from_abi(b: &abi::BlendState) -> BlendDetail {
    BlendDetail {
        enable: b.enable != 0,
        src_rgb: b.src_rgb,
        dst_rgb: b.dst_rgb,
        op_rgb: b.op_rgb,
        src_alpha: b.src_alpha,
        dst_alpha: b.dst_alpha,
        op_alpha: b.op_alpha,
    }
}

/// The draw with the answer applied, or `None` when the answer changes nothing in it. `handle_of` finds a handle
/// that this frame resolved to texture `id`. A forced slot whose draw has no bind, or whose texture has no handle in
/// this frame, stays as it is.
pub(crate) fn apply_draw(
    d: &DrawDetail,
    a: &abi::DrawAnswer,
    handle_of: &dyn Fn(u32) -> Option<u64>,
) -> Option<DrawDetail> {
    let mut out = d.clone();
    if a.flags & abi::DRAW_BLEND != 0 {
        for (slot, b) in out.blend.iter_mut().enumerate() {
            if a.blend_slots & (1 << slot) != 0 {
                *b = blend_from_abi(&a.blend[slot]);
            }
        }
    }
    if a.flags & abi::DRAW_TEXTURE != 0 {
        for f in a.forced_list() {
            let Some(handle) = handle_of(f.tex_id) else {
                continue;
            };
            for t in out
                .textures
                .iter_mut()
                .filter(|t| stage_code(t.stage) == f.stage && t.slot == f.slot)
            {
                t.handle = handle;
            }
        }
    }
    if a.flags & abi::DRAW_STENCIL_REF != 0 {
        out.dynamic.stencil_ref = a.stencil_ref;
    }
    (out != *d).then_some(out)
}

/// What the encoder does with a draw after the patches answered.
pub(crate) enum Draw {
    /// The draw as it is.
    Keep,
    /// Leave the draw out.
    Skip,
    /// Encode this draw in place of the recorded one.
    Replace(Box<DrawDetail>),
}

/// Asks the patches about draw `n` of pass `pass`. The handles of the frame (`handles`) and the textures that they
/// resolved to (`crate::diag::texture_of`) name a texture by its id.
pub(crate) fn draw(sh: &Shared<'_>, pass: usize, n: u32, d: &DrawDetail) -> Draw {
    if !draw_on() {
        return Draw::Keep;
    }
    let info = draw_info(crate::diag::frame(), pass, n, d, &crate::diag::texture_of);
    let answer = registry::draw(&info);
    if answer.flags & abi::DRAW_SKIP != 0 {
        return Draw::Skip;
    }
    let handle_of = |id: u32| {
        sh.env
            .handles
            .keys()
            .copied()
            .find(|h| crate::diag::texture_of(*h) == Some(TexId(id)))
    };
    match apply_draw(d, &answer, &handle_of) {
        Some(p) => Draw::Replace(Box::new(p)),
        None => Draw::Keep,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detail::TextureBind;

    fn draw_with_textures() -> DrawDetail {
        let mut d = crate::walk::empty_draw();
        d.textures = vec![
            TextureBind {
                stage: Stage::Fragment,
                slot: 0,
                handle: 0x100_0005,
            },
            TextureBind {
                stage: Stage::Vertex,
                slot: 0,
                handle: 0x100_0006,
            },
        ];
        d
    }

    #[test]
    fn the_default_answer_changes_nothing() {
        let d = draw_with_textures();
        assert!(apply_draw(&d, &abi::DrawAnswer::default(), &|_| Some(1)).is_none());
    }

    #[test]
    fn blend_texture_and_stencil_overrides_apply() {
        let d = draw_with_textures();
        let mut a = abi::DrawAnswer::default();
        a.set_blend(
            1,
            abi::BlendState {
                enable: 1,
                src_rgb: 2,
                ..Default::default()
            },
        );
        a.force_texture(1, 0, 42);
        a.force_texture(0, 7, 43);
        a.flags |= abi::DRAW_STENCIL_REF;
        a.stencil_ref = [9, 8];
        let got = apply_draw(&d, &a, &|id| (id == 42).then_some(0x200_0007));
        let Some(got) = got else {
            unreachable!("the answer changes the draw");
        };
        assert!(got.blend[1].enable && got.blend[1].src_rgb == 2);
        assert!(!got.blend[0].enable);
        assert_eq!(got.textures[0].handle, 0x200_0007);
        assert_eq!(got.textures[1].handle, 0x100_0006);
        assert_eq!(got.dynamic.stencil_ref, [9, 8]);
    }

    #[test]
    fn an_unknown_texture_leaves_the_slot() {
        let d = draw_with_textures();
        let mut a = abi::DrawAnswer::default();
        a.force_texture(1, 0, 42);
        assert!(apply_draw(&d, &a, &|_| None).is_none());
    }

    #[test]
    fn the_info_names_programs_and_textures() {
        let mut d = draw_with_textures();
        d.program.vertex = "vs1".into();
        d.program.fragment = Some("fs1".into());
        d.dynamic.stencil_ref = [5, 6];
        let info = draw_info(7, 2, 11, &d, &|h| (h == 0x100_0005).then_some(TexId(5)));
        assert_eq!((info.frame, info.pass, info.draw), (7, 2, 11));
        assert_eq!(info.programs.vertex.as_str(), "vs1");
        assert_eq!(info.programs.fragment.as_str(), "fs1");
        assert_eq!(info.texture_list().len(), 2);
        assert_eq!(info.texture_list()[0].tex_id, 5);
        assert_eq!(info.texture_list()[1].tex_id, abi::TEX_UNKNOWN);
        assert_eq!(info.texture_list()[0].stage, 1);
        assert_eq!(info.stencil_ref, [5, 6]);
    }

    #[test]
    fn the_load_answer_respects_slots_and_memoryless() {
        let a = abi::PassAnswer {
            flags: abi::PASS_LOAD,
            load_slots: 0b10,
            load: abi::LOAD_CLEAR,
            clear: [1.0, 0.5, 0.0, 1.0],
        };
        assert!(new_load(&a, 0, false).is_none());
        assert_eq!(
            new_load(&a, 1, true),
            Some((MLoad::Clear, [1.0, 0.5, 0.0, 1.0]))
        );
        let keep = abi::PassAnswer {
            load: abi::LOAD_KEEP,
            ..a
        };
        assert_eq!(new_load(&keep, 1, true).map(|l| l.0), Some(MLoad::DontCare));
        assert_eq!(new_load(&keep, 1, false).map(|l| l.0), Some(MLoad::Load));
        let none = abi::PassAnswer { flags: 0, ..a };
        assert!(new_load(&none, 1, false).is_none());
    }

    #[test]
    fn the_full_barrier_covers_the_render_stages() {
        let d = full_barrier();
        assert_eq!(d.after, Stages::ALL);
        assert_eq!(
            d.before,
            Stages::VERTEX | Stages::FRAGMENT | Stages::OBJECT | Stages::MESH
        );
    }
}
