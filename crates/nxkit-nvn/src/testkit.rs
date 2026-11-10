//! A test-only builder of effect streams.
//!
//! The pass analysis tests use it to write a stream of [`Cmd`] values without command memory.
//! The real path is [`crate::cmdbuf`] and [`crate::decode`].

use crate::frame::{Cmd, CmdStream, DrawState, MAX_COLOR, RtSet};
use crate::resource::TexId;

#[derive(Debug)]
pub(crate) struct Recorder {
    stream: CmdStream,
    scissor_full: bool,
    textures: [Option<TexId>; 128],
}

impl Recorder {
    pub(crate) fn new() -> Self {
        Self {
            stream: CmdStream::new(),
            scissor_full: true,
            textures: [None; 128],
        }
    }

    pub(crate) fn stream(&self) -> &CmdStream {
        &self.stream
    }

    pub(crate) fn begin(&mut self) {
        *self = Self::new();
    }

    pub(crate) fn set_render_targets(&mut self, color: &[TexId], depth: Option<TexId>) {
        let mut set = RtSet {
            depth,
            ..RtSet::default()
        };
        for (slot, t) in color.iter().take(MAX_COLOR).enumerate() {
            set.color[slot] = Some(*t);
        }
        self.stream.cmds.push(Cmd::SetRenderTargets(set));
    }

    pub(crate) fn set_scissor_full(&mut self, full: bool) {
        self.scissor_full = full;
    }

    pub(crate) fn bind_texture(&mut self, slot: usize, tex: Option<TexId>) {
        self.textures[slot] = tex;
    }

    pub(crate) fn clear_color(&mut self, slot: u8) {
        let full = self.scissor_full;
        self.stream.cmds.push(Cmd::ClearColor { slot, full });
    }

    pub(crate) fn clear_depth_stencil(&mut self) {
        let full = self.scissor_full;
        self.stream.cmds.push(Cmd::ClearDepthStencil { full });
    }

    pub(crate) fn discard(&mut self, color: u8, depth: bool) {
        self.stream.cmds.push(Cmd::Discard { color, depth });
    }

    fn reads(&mut self) -> crate::frame::Span {
        let list: Vec<TexId> = self.textures.iter().flatten().copied().collect();
        self.stream.push_list(&list)
    }

    pub(crate) fn draw(&mut self, state: DrawState) {
        let reads = self.reads();
        self.stream.cmds.push(Cmd::Draw { state, reads });
    }

    pub(crate) fn dispatch(&mut self) {
        let reads = self.reads();
        let writes = self.stream.push_list(&[]);
        self.stream.cmds.push(Cmd::Dispatch { reads, writes });
    }

    pub(crate) fn copy(&mut self, src: Option<TexId>, dst: Option<TexId>) {
        self.stream.cmds.push(Cmd::Copy { src, dst });
    }

    pub(crate) fn buffer_copy(&mut self) {
        self.stream.cmds.push(Cmd::BufferCopy);
    }

    pub(crate) fn barrier(&mut self, bits: u32) {
        self.stream.cmds.push(Cmd::Barrier(bits));
    }

    pub(crate) fn present(&mut self, tex: TexId) {
        self.stream.cmds.push(Cmd::Present(tex));
    }
}
