//! The lookups of the temporal upscale in the frame: the render passes, the bound textures, the colour attachments and
//! the view uniform buffer.

use super::*;

/// The render passes of the frame of `sh` with the range of their draws.
pub(super) fn render_passes(sh: &Shared<'_>) -> Vec<(usize, Range<u32>)> {
    sh.plan
        .passes
        .iter()
        .enumerate()
        .filter(|(_, p)| p.kind == PassKind::Render)
        .map(|(i, p)| {
            let first = sh.ordinals.get(p.cmds.start as usize).map_or(0, |o| o.draw);
            let n = p
                .cmds
                .clone()
                .filter(|c| matches!(sh.frame.cmds.get(*c as usize), Some(Cmd::Draw { .. })))
                .count();
            (i, first..first + n as u32)
        })
        .collect()
}

/// The texture that draw `d` binds at fragment slot `slot`.
pub(super) fn bound<'a>(
    sh: &Shared<'a>,
    d: &DrawDetail,
    slot: u32,
    what: &str,
) -> Result<(TexId, &'a Res), Miss> {
    let t = d
        .textures
        .iter()
        .find(|t| t.stage == Stage::Fragment && t.slot == slot && t.handle != 0)
        .ok_or_else(|| {
            why(format!(
                "the {what} draw binds no texture at fragment slot {slot}"
            ))
        })?;
    let id = sh
        .table
        .resolve(t.handle)
        .map_err(|_| why(format!("the {what} texture does not resolve")))?
        .0
        .texture;
    let res = sh
        .textures
        .get(&id)
        .ok_or_else(|| why(format!("the {what} texture has no Metal texture")))?;
    Ok((id, res))
}

/// Colour attachment 0 of pass `index`, as the pass renders into it.
pub(super) fn color0(sh: &Shared<'_>, index: usize, what: &str) -> Result<(TexId, Texture), Miss> {
    let missing = || why(format!("the {what} pass has no colour attachment 0"));
    let pass = sh.plan.passes.get(index).ok_or_else(missing)?;
    let a = sh
        .plan
        .attachments_of(pass)
        .iter()
        .find(|a| a.slot == Slot::Color(0))
        .ok_or_else(missing)?;
    let res = sh.textures.get(&a.tex).ok_or_else(missing)?;
    let t = sh.texture_store.target_view(res, a.view).map_err(|s| {
        why(format!(
            "colour attachment 0 of the {what} pass has no view ({})",
            s.name()
        ))
    })?;
    Ok((a.tex, t))
}

/// The bytes of the view uniform buffer of draw `d`.
pub(super) fn view_bytes(sh: &Shared<'_>, d: &DrawDetail, slot: UboSlot) -> Result<Vec<u8>, Miss> {
    let u = d
        .ubos
        .iter()
        .find(|u| u.stage == slot.stage && u.slot == slot.slot)
        .ok_or_else(|| {
            why(format!(
                "the source draw binds no {:?} uniform buffer at slot {}",
                slot.stage, slot.slot
            ))
        })?;
    if let Some(b) = &u.snapshot {
        return Ok(b.to_vec());
    }
    let mut b = vec![0; (u.size as usize).min(1 << 16)];
    if !sh.read_guest(u.address, &mut b) {
        return Err(why("the view uniform buffer is not readable"));
    }
    Ok(b)
}

pub(super) fn srgb(f: Format) -> bool {
    matches!(f, Format::Rgba8UnormSrgb | Format::Bgra8UnormSrgb)
}
