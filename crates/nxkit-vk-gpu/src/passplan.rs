//! How a Vulkan render pass becomes Metal render passes (the reference layer's `begin_rp`, `end_rp`, `blit` and
//! `resume_rp`). Pure functions over the recorded commands.
//!
//! - A load op of `CLEAR` clears, and `LOAD` and `DONT_CARE` load. The Switch's driver discards nothing at the begin of
//!   a pass, and games read what a `DONT_CARE` attachment held (Re:ZERO's effects layer came out black with a
//!   discarding load).
//! - A copy, a clear of an image, a dispatch or a barrier inside a render pass ends the Metal pass. The pass is a
//!   *segment*: one that the game's `vkCmdEndRenderPass` ends stores as the pass says, one that a cut ends stores
//!   everything, and the next draw resumes the pass in a new segment that loads everything.
//! - `vkCmdClearAttachments` of whole attachments is a cut too: the next segment clears those attachments with its
//!   load action.

use std::collections::HashMap;
use std::sync::Arc;

use nxkit_vk::cmd::{ClearAttachment, ClearRect, ClearValue, Cmd};
use nxkit_vk::desc::RenderPassDesc;
use nxkit_vk::format::aspect;

/// The load action of an attachment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Load {
    /// Keep what the attachment holds.
    Load,
    /// Clear it to the value.
    Clear(ClearValue),
}

/// The store action of an attachment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Store {
    /// Keep what was drawn.
    Store,
    /// Drop it.
    DontCare,
}

/// How a segment ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum End {
    /// At the game's `vkCmdEndRenderPass`: the pass's own store actions.
    Final,
    /// At a command that ends the Metal pass inside the Vulkan pass: everything is stored.
    Cut,
}

/// How a segment starts.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Start<'a> {
    /// At `vkCmdBeginRenderPass`, with its clear values.
    Begin(&'a [ClearValue]),
    /// After a cut, with the attachments that a `vkCmdClearAttachments` clears.
    Resume(&'a [ClearAttachment]),
}

/// The actions of the attachments of one segment: per colour attachment of the subpass (None for an unused one), the
/// depth and the stencil.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Actions {
    /// The colour attachments.
    pub(crate) colors: Vec<Option<(Load, Store)>>,
    /// The depth aspect of the depth-stencil attachment, when its format has depth.
    pub(crate) depth: Option<(Load, Store)>,
    /// The stencil aspect, when its format has stencil.
    pub(crate) stencil: Option<(Load, Store)>,
}

fn load_of(op: u32, value: Option<ClearValue>) -> Load {
    match (op, value) {
        (1, Some(v)) => Load::Clear(v),
        (1, None) => Load::Clear([0; 4]),
        _ => Load::Load,
    }
}

fn store_of(op: u32, end: End) -> Store {
    match end {
        End::Final if op == 1 => Store::DontCare,
        _ => Store::Store,
    }
}

/// The value that a `vkCmdClearAttachments` gives to an aspect of colour attachment `color` (None for depth and
/// stencil).
fn cleared(clears: &[ClearAttachment], aspect_bit: u32, color: Option<u32>) -> Option<ClearValue> {
    clears
        .iter()
        .rev()
        .find(|c| {
            c.aspect & aspect_bit != 0 && (color.is_none() || Some(c.color_attachment) == color)
        })
        .map(|c| c.value)
}

/// The actions of a segment of `pass`.
pub(crate) fn actions(pass: &RenderPassDesc, start: &Start<'_>, end: End) -> Actions {
    let at = |a: Option<u32>| a.and_then(|i| pass.attachments.get(i as usize).map(|d| (i, d)));
    let begin_value = |i: u32| match start {
        Start::Begin(values) => values.get(i as usize).copied(),
        Start::Resume(_) => None,
    };
    let resume = |op: u32, store: u32, v: Option<ClearValue>| match start {
        Start::Begin(_) => (load_of(op, v), store_of(store, end)),
        Start::Resume(_) => (v.map_or(Load::Load, Load::Clear), store_of(store, end)),
    };
    let colors = pass
        .colors
        .iter()
        .enumerate()
        .map(|(slot, c)| {
            at(*c).map(|(i, d)| {
                let v = match start {
                    Start::Begin(_) => begin_value(i),
                    Start::Resume(cl) => cleared(cl, aspect::COLOR, Some(slot as u32)),
                };
                resume(d.load, d.store, v)
            })
        })
        .collect();
    let (mut depth, mut stencil) = (None, None);
    if let Some((i, d)) = at(pass.depth)
        && let Some(f) = nxkit_vk::format::lookup(d.format)
    {
        let value = |bit| match start {
            Start::Begin(_) => begin_value(i),
            Start::Resume(cl) => cleared(cl, bit, None),
        };
        if f.has_depth() {
            depth = Some(resume(d.load, d.store, value(aspect::DEPTH)));
        }
        if f.has_stencil() {
            stencil = Some(resume(
                d.stencil_load,
                d.stencil_store,
                value(aspect::STENCIL),
            ));
        }
    }
    Actions {
        colors,
        depth,
        stencil,
    }
}

/// Whether a command ends the Metal pass when it comes inside a render pass.
pub(crate) fn is_cut(c: &Cmd) -> bool {
    matches!(
        c,
        Cmd::CopyBuffer { .. }
            | Cmd::CopyBufferToImage { .. }
            | Cmd::CopyImageToBuffer { .. }
            | Cmd::CopyImage { .. }
            | Cmd::BlitImage { .. }
            | Cmd::ResolveImage { .. }
            | Cmd::FillBuffer { .. }
            | Cmd::UpdateBuffer { .. }
            | Cmd::ClearColorImage { .. }
            | Cmd::ClearDepthStencilImage { .. }
            | Cmd::ClearAttachments { .. }
            | Cmd::Dispatch(_)
            | Cmd::DispatchIndirect(_)
            | Cmd::PipelineBarrier
    )
}

/// How the segment that starts at `cmds[from]` ends: at the first `vkCmdEndRenderPass` or cut after it. A command
/// list that ends inside the pass stores everything.
pub(crate) fn segment_end(cmds: &[&Cmd], from: usize) -> End {
    for c in cmds.iter().skip(from) {
        match c {
            Cmd::EndRenderPass | Cmd::BeginRenderPass(_) => return End::Final,
            c if is_cut(c) => return End::Cut,
            _ => {}
        }
    }
    End::Cut
}

/// Whether the rectangles of a `vkCmdClearAttachments` cover the whole of a render target of `w` x `h`, which a load
/// action can clear.
pub(crate) fn covers(rects: &[ClearRect], w: u32, h: u32) -> bool {
    rects.iter().any(|r| {
        r.base_layer == 0
            && r.layers >= 1
            && r.rect.x <= 0
            && r.rect.y <= 0
            && i64::from(r.rect.x) + i64::from(r.rect.width) >= i64::from(w)
            && i64::from(r.rect.y) + i64::from(r.rect.height) >= i64::from(h)
    })
}

/// The commands of a primary command buffer with the secondary command buffers that it executes put in their place,
/// two levels deep. A secondary that the submission lacks is left out.
pub(crate) fn flatten<'a>(
    cmds: &'a [Cmd],
    secondaries: &'a HashMap<u64, Arc<[Cmd]>>,
    out: &mut Vec<&'a Cmd>,
    depth: u32,
) {
    for c in cmds {
        match c {
            Cmd::ExecuteCommands(handles) if depth < 2 => {
                for h in handles {
                    if let Some(inner) = secondaries.get(h) {
                        flatten(inner, secondaries, out, depth + 1);
                    }
                }
            }
            Cmd::ExecuteCommands(_) => {}
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use nxkit_vk::desc::{AttachmentDesc, Rect2D};

    use super::*;

    /// A pass with a colour attachment (clear, store), an unused slot, a second colour (don't care, don't care) and a
    /// depth-stencil attachment (load / store for depth, clear / don't care for stencil).
    fn pass() -> RenderPassDesc {
        let a = |format, load, store, stencil_load, stencil_store| AttachmentDesc {
            format,
            samples: 1,
            load,
            store,
            stencil_load,
            stencil_store,
        };
        RenderPassDesc {
            attachments: vec![a(37, 1, 0, 2, 1), a(44, 2, 1, 2, 1), a(130, 0, 0, 1, 1)],
            colors: vec![Some(0), None, Some(1)],
            resolves: vec![],
            depth: Some(2),
        }
    }

    #[test]
    fn a_begin_clears_or_loads_and_dont_care_loads() {
        let clear = [[1; 4], [2; 4], [3; 4]];
        let a = actions(&pass(), &Start::Begin(&clear), End::Final);
        assert_eq!(
            a.colors,
            vec![
                Some((Load::Clear([1; 4]), Store::Store)),
                None,
                Some((Load::Load, Store::DontCare)),
            ]
        );
        assert_eq!(a.depth, Some((Load::Load, Store::Store)));
        assert_eq!(a.stencil, Some((Load::Clear([3; 4]), Store::DontCare)));
    }

    #[test]
    fn a_cut_stores_everything_and_the_resume_loads() {
        let a = actions(&pass(), &Start::Begin(&[]), End::Cut);
        assert!(a.colors.iter().flatten().all(|(_, s)| *s == Store::Store));
        assert_eq!(a.stencil.map(|s| s.1), Some(Store::Store));
        let r = actions(&pass(), &Start::Resume(&[]), End::Final);
        assert!(r.colors.iter().flatten().all(|(l, _)| *l == Load::Load));
        assert_eq!(r.colors[2].map(|c| c.1), Some(Store::DontCare));
        assert_eq!(r.depth, Some((Load::Load, Store::Store)));
    }

    #[test]
    fn a_resume_after_clear_attachments_clears_what_they_name() {
        let clears = [
            ClearAttachment {
                aspect: aspect::COLOR,
                color_attachment: 2,
                value: [9; 4],
            },
            ClearAttachment {
                aspect: aspect::DEPTH,
                color_attachment: 0,
                value: [5; 4],
            },
        ];
        let r = actions(&pass(), &Start::Resume(&clears), End::Final);
        assert_eq!(r.colors[0].map(|c| c.0), Some(Load::Load));
        assert_eq!(r.colors[2].map(|c| c.0), Some(Load::Clear([9; 4])));
        assert_eq!(r.depth.map(|d| d.0), Some(Load::Clear([5; 4])));
        assert_eq!(r.stencil.map(|d| d.0), Some(Load::Load));
    }

    #[test]
    fn segments_end_at_the_end_of_the_pass_or_at_a_cut() {
        let draw = Cmd::Draw {
            vertices: 3,
            instances: 1,
            first_vertex: 0,
            first_instance: 0,
        };
        let copy = Cmd::FillBuffer {
            dst: Default::default(),
            data: 0,
        };
        let end = Cmd::EndRenderPass;
        assert_eq!(segment_end(&[&draw, &draw, &end], 0), End::Final);
        assert_eq!(segment_end(&[&draw, &copy, &draw, &end], 0), End::Cut);
        assert_eq!(segment_end(&[&draw, &copy, &draw, &end], 2), End::Final);
        assert_eq!(segment_end(&[&draw], 0), End::Cut);
        assert!(is_cut(&Cmd::PipelineBarrier) && !is_cut(&draw));
    }

    #[test]
    fn whole_clears_are_found() {
        let r = |x, y, width, height, base_layer| ClearRect {
            rect: Rect2D {
                x,
                y,
                width,
                height,
            },
            base_layer,
            layers: 1,
        };
        assert!(covers(&[r(0, 0, 64, 32, 0)], 64, 32));
        assert!(covers(&[r(8, 8, 4, 4, 0), r(0, 0, 100, 100, 0)], 64, 32));
        assert!(!covers(&[r(0, 0, 63, 32, 0)], 64, 32));
        assert!(!covers(&[r(0, 0, 64, 32, 1)], 64, 32));
    }

    #[test]
    fn secondaries_are_replayed_in_place() {
        let a = Cmd::SetLineWidth(1.0);
        let b = Cmd::SetLineWidth(2.0);
        let inner: Arc<[Cmd]> = Arc::from(vec![b.clone()]);
        let secondaries = HashMap::from([(5u64, inner)]);
        let primary = vec![a.clone(), Cmd::ExecuteCommands(vec![5, 6]), a.clone()];
        let mut out = Vec::new();
        flatten(&primary, &secondaries, &mut out, 0);
        assert_eq!(out, vec![&a, &b, &a]);
    }
}
