//! The effect of an extension record on a summary.
//!
//! Most extension records are state for the backend that replays the recording. They change nothing that the pass
//! analysis reads. A draw, a dispatch, a clear, a copy and a sync act like the records of the same name. A state bind
//! patches the draw state, and a scissor tells whether it covers the whole target.

use super::{Builder, Ctx};
use crate::ext::{Ext, ExtCode, Setting, StateKind};
use crate::frame::{Cmd, MAX_COLOR, TargetView};
use crate::marks::{Mark, MarkOp};
use crate::mem::GuestMem;
use crate::record::Record;

/// The scissor index that decides whether the scissor covers the target. The others are state for the replay.
const FIRST_SCISSOR: u16 = 0;

impl Builder {
    pub(super) fn apply_ext<M: GuestMem>(&mut self, cx: &Ctx<M>, x: Ext) {
        match x.code {
            ExtCode::TargetView if usize::from(x.arg) <= MAX_COLOR => {
                self.needs.targets |= !self.sets.targets;
                self.st.targets.views[usize::from(x.arg)] =
                    TargetView::from_words([x.word(0), x.word(1), x.word(2), x.word(3)]);
                self.sets.targets = true;
                self.blockers |= super::BLOCK_TARGETS;
                self.stream
                    .cmds
                    .push(Cmd::SetRenderTargets(self.st.targets));
            }
            ExtCode::Draw => self.apply(cx, Record::Draw),
            ExtCode::Dispatch => self.apply(cx, Record::Dispatch),
            ExtCode::ClearColor => {
                let slot = (x.arg & 0xff) as u8;
                if usize::from(slot) < MAX_COLOR {
                    let partial = x.word(0) & 0xf != 0xf;
                    self.clear(cx, partial, Record::ClearColor { slot });
                }
            }
            ExtCode::ClearDepthStencil => self.apply(cx, Record::ClearDepthStencil),
            ExtCode::ClearBuffer | ExtCode::CopyBufferToBuffer => {
                self.blockers |= super::BLOCK_COPY;
                self.stream.cmds.push(Cmd::BufferCopy);
            }
            ExtCode::UpdateUniform => {
                self.apply(cx, Record::Copy { src: 0, dst: 0 });
            }
            ExtCode::ClearTexture | ExtCode::CopyBufferToTexture => {
                let dst = x.word64(0);
                self.apply(cx, Record::Copy { src: 0, dst });
            }
            ExtCode::CopyTextureToBuffer => {
                let src = x.word64(0);
                self.apply(cx, Record::Copy { src, dst: 0 });
            }
            ExtCode::CopyTextureToTexture => {
                let (src, dst) = (x.word64(0), x.word64(2));
                self.apply(cx, Record::Copy { src, dst });
            }
            ExtCode::DownsampleView => {
                let (src, dst) = (x.word64(0), x.word64(2));
                self.apply(cx, Record::Downsample { src, dst });
            }
            ExtCode::FenceSync => {
                self.mark(MarkOp::FenceSync { sync: x.word64(0) });
                self.apply(cx, Record::Sync);
            }
            ExtCode::SignalEvent => {
                self.mark(MarkOp::SignalEvent {
                    event: x.word64(0),
                    mode: x.word(2),
                    value: x.word(5),
                });
                self.apply(cx, Record::Sync);
            }
            // A wait adds no command: it orders queues (the host holds the submit), and a command would cut the pass.
            ExtCode::WaitSync => {
                self.blockers |= super::BLOCK_SYNC;
                self.mark(MarkOp::WaitSync { sync: x.word64(0) });
            }
            ExtCode::WaitEvent => {
                self.blockers |= super::BLOCK_SYNC;
                self.mark(MarkOp::WaitEvent {
                    event: x.word64(0),
                    mode: x.word(2),
                    value: x.word(3),
                });
            }
            ExtCode::ReportValue => {
                self.blockers |= super::BLOCK_SYNC;
                self.mark(MarkOp::ReportValue {
                    addr: x.word64(1),
                    value: x.word(0),
                });
            }
            ExtCode::ReportCounter => {
                self.blockers |= super::BLOCK_SYNC;
                self.mark(MarkOp::ReportCounter {
                    addr: x.word64(1),
                    kind: x.word(0),
                });
            }
            ExtCode::ResetCounter => {
                self.blockers |= super::BLOCK_SYNC;
                self.mark(MarkOp::ResetCounter { kind: x.word(0) });
            }
            // A recording under a render condition is never an indirect command buffer: some of its draws may not run.
            ExtCode::Setting if x.arg == Setting::RenderEnable as u16 => {
                self.blockers |= super::BLOCK_SYNC;
                self.mark(MarkOp::RenderEnable(x.word(0) != 0));
            }
            ExtCode::Setting if x.arg == Setting::RenderEnableConditional as u16 => {
                self.blockers |= super::BLOCK_SYNC;
                self.mark(MarkOp::RenderIf {
                    mode: x.word(0),
                    addr: x.word64(1),
                });
            }
            ExtCode::State => self.patch_state(x),
            ExtCode::Scissor if x.arg == FIRST_SCISSOR => self.scissor(cx, x),
            _ => {}
        }
    }

    /// Puts a mark before the next command.
    fn mark(&mut self, op: MarkOp) {
        let at = self.stream.cmds.len() as u32;
        self.stream.marks.push(Mark { at, op });
    }

    /// A clear that does not cover the whole target is a partial clear, whatever the scissor says.
    fn clear<M: GuestMem>(&mut self, cx: &Ctx<M>, partial: bool, rec: Record) {
        let full = self.st.scissor_full;
        if partial {
            self.st.scissor_full = false;
        }
        self.apply(cx, rec);
        self.st.scissor_full = full;
    }

    fn patch_state(&mut self, x: Ext) {
        let patch = (x.arg >> 8) as u8;
        let Some(kind) = StateKind::from_u8(x.arg as u8) else {
            return;
        };
        let d = &mut self.st.draw;
        match kind {
            StateKind::Color => d.blend = patch & 1 != 0,
            StateKind::ChannelMask => d.color_write = patch,
            StateKind::DepthStencil => {
                d.depth_test = patch & 1 != 0;
                d.depth_write = patch & 2 != 0;
                d.stencil_test = patch & 4 != 0;
            }
            StateKind::Blend | StateKind::Multisample | StateKind::Polygon => return,
        }
        self.needs.draw |= !self.sets.draw;
        self.sets.draw = true;
    }

    /// The scissor covers the target when it starts at or before the origin and reaches the far edges. Without a
    /// target the answer is no.
    fn scissor<M: GuestMem>(&mut self, cx: &Ctx<M>, x: Ext) {
        self.needs.targets |= !self.sets.targets;
        let t = &self.st.targets;
        let target = t
            .color
            .iter()
            .enumerate()
            .find_map(|(slot, id)| id.map(|id| (slot, id)))
            .or(t.depth.map(|id| (MAX_COLOR, id)));
        let dims = target.and_then(|(slot, id)| {
            cx.table.get(id).ok().map(|r| {
                let (w, h, _) = r.desc.level_extent(t.views[slot].level());
                (w, h)
            })
        });
        let (x0, y0) = (x.word(0) as i32, x.word(1) as i32);
        let (w, h) = (x.word(2) as i32, x.word(3) as i32);
        self.st.scissor_full = dims.is_some_and(|(tw, th)| {
            x0 <= 0 && y0 <= 0 && i64::from(w) >= i64::from(tw) && i64::from(h) >= i64::from(th)
        });
        self.sets.scissor = true;
    }
}
