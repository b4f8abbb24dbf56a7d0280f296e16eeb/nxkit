//! The `nanscan` count of the float storage images that the dispatches of a compute pass bind.

use super::*;

/// A float storage image that the dispatches of a compute pass bind.
struct Bound {
    id: TexId,
    tex: Texture,
    slot: u32,
    /// The compute program keys that bind it, in order of first use.
    keys: Vec<String>,
    /// The record of the count before the pass.
    load: Option<usize>,
}

/// What `compute_before` found for a compute pass and `ComputeScan::after` needs.
pub(crate) struct ComputeScan {
    pass: usize,
    images: Vec<Bound>,
}

/// Collects the float storage images of the dispatches of the compute pass `pass` (number `index`) and encodes their
/// count before the pass. It has to run before the compute pass of the game begins. None when nothing is scanned: the
/// category is off, or neither `NXKIT_DIAG_FS` nor the knob `nancomp` selects a dispatch.
pub(crate) fn compute_before(
    sh: &Shared<'_>,
    cb: &mut CommandBuffer,
    index: usize,
    pass: &Pass,
) -> Option<ComputeScan> {
    if !active() {
        return None;
    }
    let listed = crate::diag::fs_programs();
    let all = crate::diag::knob("nancomp");
    if listed.is_empty() && !all {
        return None;
    }
    let mut images: Vec<Bound> = Vec::new();
    // The textures that listed dispatches sample: key, slot and id.
    let mut sampled: Vec<(String, u32, TexId)> = Vec::new();
    for i in pass.cmds.clone() {
        if sh.frame.skips(i) {
            continue;
        }
        let ord = sh.ordinals.get(i as usize).copied().unwrap_or_default();
        let Some(dd) = sh.detail.dispatches.get(ord.dispatch as usize) else {
            continue;
        };
        let is_listed = listed.contains(&dd.program);
        if !all && !is_listed {
            continue;
        }
        if is_listed {
            for t in dd
                .textures
                .iter()
                .filter(|t| t.handle != 0 && t.stage == Stage::Compute)
            {
                if let Some(id) = crate::barr::texture_of(t.handle) {
                    sampled.push((dd.program.clone(), t.slot, id));
                }
            }
        }
        for t in dd.images.iter().filter(|t| t.handle != 0) {
            let Some(id) = crate::barr::texture_of(t.handle) else {
                continue;
            };
            let Some(res) = sh.textures.get(&id) else {
                continue;
            };
            if !float_format(res.desc.format) {
                continue;
            }
            match images.iter_mut().find(|b| b.id == id) {
                Some(b) => {
                    if !b.keys.contains(&dd.program) {
                        b.keys.push(dd.program.clone());
                    }
                }
                None => images.push(Bound {
                    id,
                    tex: res.tex.clone(),
                    slot: t.slot,
                    keys: vec![dd.program.clone()],
                    load: None,
                }),
            }
        }
    }
    for (key, slot, id) in sampled {
        let Some(res) = sh.textures.get(&id) else {
            continue;
        };
        if !float_format(res.desc.format)
            || images.iter().any(|b| {
                b.id == id
                    || sh
                        .textures
                        .get(&b.id)
                        .is_some_and(|r| r.barrier_id == res.barrier_id)
            })
        {
            continue;
        }
        let first = STATE
            .lock()
            .ok()
            .and_then(|mut st| st.as_mut().map(|s| s.seen.insert(id)))
            .unwrap_or(false);
        if first {
            let kind = Kind::Sample { stage: "cs", slot };
            scan_texture(sh, cb, index, id, &res.tex, &key, kind);
        }
    }
    if images.is_empty() {
        return None;
    }
    for b in &mut images {
        let prog = prog_text(&b.keys);
        b.load = scan_texture(sh, cb, index, b.id, &b.tex, &prog, Kind::Load);
    }
    Some(ComputeScan {
        pass: index,
        images,
    })
}

/// The program keys of a line, at most `KEYS`.
fn prog_text(keys: &[String]) -> String {
    keys.iter()
        .take(KEYS)
        .cloned()
        .collect::<Vec<_>>()
        .join(",")
}

impl ComputeScan {
    /// Encodes the count of the images, right after the compute pass.
    pub(crate) fn after(self, sh: &Shared<'_>, cb: &mut CommandBuffer) {
        for b in self.images {
            let kind = Kind::Image {
                slot: b.slot,
                load: b.load,
            };
            scan_texture(sh, cb, self.pass, b.id, &b.tex, &prog_text(&b.keys), kind);
        }
    }
}
