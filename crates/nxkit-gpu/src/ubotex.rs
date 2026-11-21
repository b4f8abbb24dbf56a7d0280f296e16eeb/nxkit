//! Bindless textures whose handle a shader reads from a uniform buffer (`tex_ubo` and `samp_ubo` of `_b.json`).
//!
//! Such a shader reads a 32-bit texture handle from uniform slot `reg` at byte `off` (the bindless `TEX` of the console,
//! with the handle in a constant buffer) and samples the texture it names. The member of the argument buffer holds the
//! resource ID of that texture, so the host reads the same word for each draw and dispatch before the frame resolves its
//! handles ([`augment`]). Each distinct source of a stage gets a texture slot of its own above the real slots
//! ([`SLOT_BASE`] plus its index): `programs` reads the member as a `tex` or `samp_tex` member of that slot, and the frame
//! gets one [`TextureBind`] per source with the handle read. From there on the handle goes the way of a bound texture:
//! it is resolved once a frame, fitted to the declared view and scaled, and the `samp_ubo` members take its sampler.
//!
//! A split handle (texture and sampler index in two words, the separate sampler handles of NVN) takes the texture index
//! from the texture word and the sampler index from the sampler word. A sampler word in the driver bank c2 (`sreg` 255)
//! is not read: such a member is refused, as before.
//!
//! The frame's analysis (`nxkit-nvn`) does not see these textures: a texture that an earlier pass of the frame renders
//! and a later draw reads only through a uniform buffer gets no barrier of its own, as with the texture pool.

use std::sync::Arc;

use crate::detail::{FrameDetail, Stage, TextureBind, UboDetail};
use crate::json::Json;

/// The first texture slot of the sources. The real texture slots of a stage are 0 to 31.
pub(crate) const SLOT_BASE: u32 = 0x1000;

/// The `sreg` of a sampler word in the driver bank c2 (`nxb_xlate::msl::abi::SREG_C2`).
const SREG_C2: i64 = 255;

/// The bits of the texture index in a handle; the sampler index is in the bits above.
const TEXTURE_BITS: u32 = 0x000F_FFFF;

/// Where the two words of a bindless handle are: (uniform slot, byte offset) of the texture index and of the sampler
/// index. They are the same word for a combined handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Source {
    pub(crate) tex: (u32, u32),
    pub(crate) samp: (u32, u32),
}

/// The source of a `tex_ubo` member (`sampler` false) or a `samp_ubo` member. `None` when `reg` or `off` is missing, or
/// when the other word is in c2.
pub(crate) fn source(b: &Json, sampler: bool) -> Option<Source> {
    let int = |k: &str| {
        b.get(k)
            .and_then(Json::as_i64)
            .and_then(|v| u32::try_from(v).ok())
    };
    let own = (int("reg")?, int("off")?);
    let other = match (b.get("sreg").and_then(Json::as_i64), int("soff")) {
        (Some(SREG_C2), _) => return None,
        (Some(r), Some(o)) => (u32::try_from(r).ok()?, o),
        _ => own,
    };
    // A `samp_ubo` member names the sampler word first and the texture word as its other half.
    Some(if sampler {
        Source {
            tex: other,
            samp: own,
        }
    } else {
        Source {
            tex: own,
            samp: other,
        }
    })
}

/// The handle of a texture word and a sampler word, in the form of [`TextureBind::handle`].
pub(crate) fn combine(tex: u32, samp: u32) -> u64 {
    u64::from((tex & TEXTURE_BITS) | (samp & !TEXTURE_BITS))
}

/// Reads the 32-bit word at byte `off` of uniform slot `slot` of `stage`: from the snapshot of the draw when it has one,
/// else from guest memory through `read`.
fn word(
    ubos: &[UboDetail],
    stage: Stage,
    (slot, off): (u32, u32),
    read: &mut dyn FnMut(u64, &mut [u8]) -> bool,
) -> Option<u32> {
    let u = ubos.iter().find(|u| u.stage == stage && u.slot == slot)?;
    if off.checked_add(4)? > u.size {
        return None;
    }
    let mut w = [0u8; 4];
    match &u.snapshot {
        Some(bytes) => w.copy_from_slice(bytes.get(off as usize..off as usize + 4)?),
        None => {
            if !read(u.address.checked_add(u64::from(off))?, &mut w) {
                return None;
            }
        }
    }
    Some(u32::from_le_bytes(w))
}

/// The `tex_ubo` and `samp_ubo` members of one stage program as (texture slot, source), several members per source.
pub(crate) type Members = Vec<(u32, Source)>;

/// Puts the members of the stage program with this key (vertex or not) into the list. `programs` gives it, so that
/// this module does not depend on `programs`.
pub(crate) type Lookup<'a> = &'a mut dyn FnMut(&str, bool, &mut Members);

/// Adds to `out` one bind for each source of the members of a stage that `out` does not have yet.
fn binds_of(
    members: &[(u32, Source)],
    stage: Stage,
    ubos: &[UboDetail],
    read: &mut dyn FnMut(u64, &mut [u8]) -> bool,
    out: &mut Vec<TextureBind>,
) {
    for &(slot, src) in members {
        if out.iter().any(|t| t.stage == stage && t.slot == slot) {
            continue;
        }
        let tex = word(ubos, stage, src.tex, read);
        let samp = if src.samp == src.tex {
            tex
        } else {
            word(ubos, stage, src.samp, read)
        };
        let handle = match (tex, samp) {
            (Some(t), Some(s)) => combine(t, s),
            _ => {
                nxkit_nvn::diag::note(
                    nxkit_nvn::diag::Reason::UboHandleUnread,
                    format_args!("{stage:?} slot {} at {}", src.tex.0, src.tex.1),
                );
                0
            }
        };
        out.push(TextureBind {
            stage,
            slot,
            handle,
        });
    }
}

/// The frame with the binds of [`binds_of`] added to every draw and dispatch whose shaders read a handle from a uniform
/// buffer, or `None` when no shader of the frame does. `read` reads guest memory as the frame sees it.
pub(crate) fn augment(
    detail: &FrameDetail,
    lookup: Lookup<'_>,
    read: &mut dyn FnMut(u64, &mut [u8]) -> bool,
) -> Option<FrameDetail> {
    let mut out: Option<FrameDetail> = None;
    let mut extra: Vec<TextureBind> = Vec::new();
    let mut members = Members::new();
    for (i, d) in detail.draws.iter().enumerate() {
        extra.clear();
        let p = &d.program;
        let stages = [
            (
                Some(&*p.vertex).filter(|k| !k.is_empty()),
                true,
                Stage::Vertex,
            ),
            (p.fragment.as_deref(), false, Stage::Fragment),
            (p.task.as_deref(), false, Stage::Task),
            (p.mesh.as_deref(), false, Stage::Mesh),
        ];
        for (key, vertex, stage) in stages {
            let Some(key) = key else { continue };
            members.clear();
            lookup(key, vertex, &mut members);
            binds_of(&members, stage, &d.ubos, read, &mut extra);
        }
        if extra.is_empty() {
            continue;
        }
        let f = out.get_or_insert_with(|| detail.clone());
        if let Some(dd) = f.draws.get_mut(i) {
            Arc::make_mut(dd).textures.extend_from_slice(&extra);
        }
    }
    for (i, d) in detail.dispatches.iter().enumerate() {
        if d.program.is_empty() {
            continue;
        }
        members.clear();
        lookup(&d.program, false, &mut members);
        extra.clear();
        binds_of(&members, Stage::Compute, &d.ubos, read, &mut extra);
        if extra.is_empty() {
            continue;
        }
        let f = out.get_or_insert_with(|| detail.clone());
        if let Some(dd) = f.dispatches.get_mut(i) {
            dd.textures.extend_from_slice(&extra);
        }
    }
    out
}

#[cfg(test)]
#[path = "ubotex_tests.rs"]
mod tests;
