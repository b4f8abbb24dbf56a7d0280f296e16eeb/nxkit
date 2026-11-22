//! The hook of the render passes (`probe::RenderHook`): it records each pass of a captured frame and, for a capture of
//! the passes, copies the attachments right after the pass on the GPU, so that a later pass cannot overwrite them
//! before the read back. A frame without a capture costs one atomic load per pass.

use std::ops::Range;

use nxkit_metal::{
    ArgumentTable, CommandBuffer, ComputeCommand, Context, Dependency, LoadAction as MLoad,
    RenderPassDesc, Scope, Stages, Storage, StoreAction as MStore, Texture, TextureKind,
    TextureRegion, Use, Visibility,
};
use nxkit_nvn::passes::{AttachmentPlan, Pass, Slot};
use nxkit_pipestate::nvn::{blend_factor, blend_op};

use super::data::{AttRec, BlendRec, PassRec};
use crate::drawctx::Shared;
use crate::probe::PassWatch;

/// The most bytes of snapshots that one capture holds on the GPU. An attachment past it gets no picture.
pub(super) const BUDGET: u64 = 2 << 30;

/// An attachment to copy after its pass.
struct Target {
    pass: usize,
    label: String,
    tex_id: u32,
    tex: Texture,
    depth_plane: u32,
}

struct Watch {
    targets: Vec<Target>,
}

/// Records render pass `index` of a captured frame. Returns what runs after the pass, when attachments are to be copied.
pub(crate) fn before(
    sh: &Shared<'_>,
    index: usize,
    pass: &Pass,
    draws: Range<u32>,
    desc: &RenderPassDesc,
) -> Option<Box<dyn PassWatch>> {
    if !super::hooks_on() {
        return None;
    }
    let pictures = super::wants_pictures();
    let mut rec = PassRec {
        index,
        kind: "Render".to_owned(),
        cmds: [pass.cmds.start, pass.cmds.end],
        draws: draws.len() as u32,
        ..PassRec::default()
    };
    let mut targets = Vec::new();
    for a in sh.plan.attachments_of(pass) {
        let (label, tex, load, store, clear, mem, plane) = match a.slot {
            Slot::Color(n) => {
                let Some(Some(c)) = desc.colors.get(usize::from(n)) else {
                    continue;
                };
                let clear = (c.load == MLoad::Clear).then(|| c.clear.to_vec());
                (
                    format!("c{n}"),
                    &c.texture,
                    c.load,
                    c.store,
                    clear,
                    c.memoryless,
                    c.depth_plane,
                )
            }
            Slot::Depth => {
                let Some(d) = &desc.depth else { continue };
                let clear = (d.load == MLoad::Clear).then(|| vec![d.clear]);
                (
                    "depth".to_owned(),
                    &d.texture,
                    d.load,
                    d.store,
                    clear,
                    d.memoryless,
                    0,
                )
            }
        };
        let td = tex.desc();
        let skipped = blocker(a, tex, store, mem);
        rec.attachments.push(AttRec {
            label: label.clone(),
            tex: a.tex.0,
            format: format!("{:?}", td.format),
            width: td.width,
            height: td.height,
            load: format!("{load:?}"),
            store: format!("{store:?}"),
            clear,
            skipped: skipped.clone(),
        });
        if pictures && skipped.is_none() {
            targets.push(Target {
                pass: index,
                label,
                tex_id: a.tex.0,
                tex: tex.clone(),
                depth_plane: plane,
            });
        }
    }
    let slots: Vec<usize> = desc
        .colors
        .iter()
        .enumerate()
        .filter(|(_, c)| c.is_some())
        .map(|(i, _)| i)
        .collect();
    for n in draws {
        let Some(d) = sh.detail.draws.get(n as usize) else {
            continue;
        };
        push_key(&mut rec.vertex, &d.program.vertex);
        for (list, key) in [
            (&mut rec.fragment, &d.program.fragment),
            (&mut rec.task, &d.program.task),
            (&mut rec.mesh, &d.program.mesh),
        ] {
            if let Some(k) = key {
                push_key(list, k);
            }
        }
        for slot in slots.iter().copied().filter(|s| *s < 8) {
            let b = &d.blend[slot];
            let state = BlendRec {
                enable: b.enable,
                src_rgb: format!("{:?}", blend_factor(b.src_rgb)),
                dst_rgb: format!("{:?}", blend_factor(b.dst_rgb)),
                op_rgb: format!("{:?}", blend_op(b.op_rgb)),
                src_alpha: format!("{:?}", blend_factor(b.src_alpha)),
                dst_alpha: format!("{:?}", blend_factor(b.dst_alpha)),
                op_alpha: format!("{:?}", blend_op(b.op_alpha)),
                write_mask: d.write_masks[slot],
            };
            note_blend(&mut rec.blend, slot as u8, state);
        }
    }
    super::record(rec);
    (!targets.is_empty()).then(|| Box::new(Watch { targets }) as Box<dyn PassWatch>)
}

fn push_key(list: &mut Vec<String>, key: &str) {
    if !key.is_empty() && !list.iter().any(|k| k == key) {
        list.push(key.to_owned());
    }
}

fn note_blend(all: &mut Vec<(u8, Vec<BlendRec>)>, slot: u8, state: BlendRec) {
    let i = match all.iter().position(|(s, _)| *s == slot) {
        Some(i) => i,
        None => {
            all.push((slot, Vec::new()));
            all.len() - 1
        }
    };
    if !all[i].1.contains(&state) {
        all[i].1.push(state);
    }
}

/// Why the attachment gets no copy, or None when it does.
fn blocker(a: &AttachmentPlan, tex: &Texture, store: MStore, memoryless: bool) -> Option<String> {
    let d = tex.desc();
    let why = if memoryless || d.storage == Storage::Memoryless {
        "memoryless attachment: its pixels never reach memory"
    } else if store != MStore::Store {
        "the pass does not store the attachment (store action DontCare), so its pixels are gone after the pass"
    } else if a.is_untouched() {
        "no draw or clear of the pass wrote the attachment: it holds what the pass before left"
    } else if tex.sample_count() > 1 {
        "multisample texture: it has no copy to a single sample picture"
    } else if d.format.is_compressed() {
        "compressed format"
    } else if d.format.has_stencil() {
        "format with a stencil aspect: the copy of its depth plane is not made"
    } else {
        return None;
    };
    Some(why.to_owned())
}

impl PassWatch for Watch {
    fn after(self: Box<Self>, sh: &Shared<'_>, cb: &mut CommandBuffer) {
        for t in self.targets {
            if let Err(why) = copy(sh.ctx, cb, &t) {
                super::skip(t.pass, &t.label, &why);
            }
        }
    }
}

/// Encodes the copy of one attachment into a new texture of the same format, and keeps the copy.
fn copy(ctx: &Context, cb: &mut CommandBuffer, t: &Target) -> Result<(), String> {
    let d = t.tex.desc();
    let bytes = u64::from(d.width) * u64::from(d.height) * u64::from(d.format.bytes_per_pixel());
    if !super::reserve(bytes) {
        return Err(format!(
            "the snapshots of the capture reached {} MiB",
            BUDGET >> 20
        ));
    }
    let mut sd = d;
    sd.kind = TextureKind::D2;
    sd.depth = 1;
    sd.array_layers = 1;
    sd.mip_levels = 1;
    sd.storage = Storage::Private;
    sd.render_target = false;
    sd.shader_read = true;
    sd.shader_write = false;
    sd.shader_atomic = false;
    sd.allow_views = false;
    let snapshot = Texture::new(ctx, sd).map_err(|e| format!("no texture for the copy: {e}"))?;
    let table = ArgumentTable::new(ctx, 1, 1).map_err(|e| format!("no table for the copy: {e}"))?;
    let mut cp = cb
        .begin_compute_pass(&table)
        .map_err(|e| format!("no pass for the copy: {e}"))?;
    let _ = cp.encode(&ComputeCommand::Depend(Dependency::between(
        Use::AttachmentWrite,
        Use::CopyRead,
    )));
    let copied = cp.encode(&ComputeCommand::CopyTexture {
        src: t.tex.clone(),
        dst: snapshot.clone(),
        src_region: TextureRegion {
            level: 0,
            slice: 0,
            origin: [0, 0, t.depth_plane],
            size: [d.width, d.height, 1],
        },
        dst_level: 0,
        dst_slice: 0,
        dst_origin: [0, 0, 0],
    });
    // Later passes may write the attachment again: they wait for the copy.
    let _ = cp.encode(&ComputeCommand::Depend(Dependency {
        scope: Scope::ToLaterPasses,
        after: Stages::BLIT,
        before: Stages::ALL,
        visibility: Visibility::Device,
    }));
    cp.end();
    copied.map_err(|e| format!("the copy was refused: {e}"))?;
    super::keep(super::Snap {
        pass: t.pass,
        label: t.label.clone(),
        tex: t.tex_id,
        format: d.format,
        width: d.width,
        height: d.height,
        snapshot,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(src: u32) -> BlendRec {
        BlendRec {
            enable: true,
            src_rgb: format!("f{src}"),
            dst_rgb: "Zero".into(),
            op_rgb: "Add".into(),
            src_alpha: "One".into(),
            dst_alpha: "Zero".into(),
            op_alpha: "Add".into(),
            write_mask: 15,
        }
    }

    #[test]
    fn a_blend_state_is_listed_once_for_each_slot() {
        let mut all = Vec::new();
        note_blend(&mut all, 0, state(1));
        note_blend(&mut all, 0, state(1));
        note_blend(&mut all, 0, state(2));
        note_blend(&mut all, 1, state(1));
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].1.len(), 2);
        assert_eq!(all[1].1.len(), 1);
    }

    #[test]
    fn program_keys_keep_the_order_and_skip_empty_and_repeated_ones() {
        let mut keys = Vec::new();
        for k in ["b", "", "a", "b"] {
            push_key(&mut keys, k);
        }
        assert_eq!(keys, ["b", "a"]);
    }
}
