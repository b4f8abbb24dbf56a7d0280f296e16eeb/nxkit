//! The recorder of one command buffer.
//!
//! The recorder keeps the state that NVN keeps per command buffer: the attachments, the scissor and the bound textures.
//! It turns the NVN calls into [`Cmd`] values in its [`CmdStream`]. A texture list is stored only when the bindings
//! changed since the last draw, so a run of draws with the same bindings shares one list.

use crate::frame::{Cmd, CmdStream, DrawState, MAX_COLOR, RtSet, Span};
use crate::resource::TexId;

/// The number of texture and image binding slots that the recorder tracks.
pub const MAX_BINDINGS: usize = 128;

/// The record of one command buffer.
#[derive(Debug)]
pub struct Recorder {
    stream: CmdStream,
    targets: RtSet,
    scissor_full: bool,
    textures: [Option<TexId>; MAX_BINDINGS],
    images: [Option<TexId>; MAX_BINDINGS],
    texture_list: Option<Span>,
    image_list: Option<Span>,
    scratch: Vec<TexId>,
}

impl Default for Recorder {
    fn default() -> Self {
        Self {
            stream: CmdStream::new(),
            targets: RtSet::default(),
            scissor_full: true,
            textures: [None; MAX_BINDINGS],
            images: [None; MAX_BINDINGS],
            texture_list: None,
            image_list: None,
            scratch: Vec::new(),
        }
    }
}

fn collect(slots: &[Option<TexId>], out: &mut Vec<TexId>) {
    out.clear();
    for t in slots.iter().flatten() {
        if !out.contains(t) {
            out.push(*t);
        }
    }
}

impl Recorder {
    /// An empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// The commands so far.
    pub fn stream(&self) -> &CmdStream {
        &self.stream
    }

    /// `nvnCommandBufferBeginRecording`. It forgets the commands and the state.
    pub fn begin(&mut self) {
        self.stream.clear();
        self.targets = RtSet::default();
        self.scissor_full = true;
        self.textures = [None; MAX_BINDINGS];
        self.images = [None; MAX_BINDINGS];
        self.texture_list = None;
        self.image_list = None;
    }

    /// `nvnCommandBufferSetRenderTargets`.
    pub fn set_render_targets(&mut self, color: &[TexId], depth: Option<TexId>) {
        let mut set = RtSet {
            depth,
            ..RtSet::default()
        };
        for (slot, t) in color.iter().take(MAX_COLOR).enumerate() {
            set.color[slot] = Some(*t);
        }
        self.targets = set;
        self.stream.cmds.push(Cmd::SetRenderTargets(set));
    }

    /// Tells whether the scissor covers the whole target. A clear under a smaller scissor is not a full clear.
    pub fn set_scissor_full(&mut self, full: bool) {
        self.scissor_full = full;
    }

    /// Binds a texture for sampling in a slot, or unbinds it.
    pub fn bind_texture(&mut self, slot: usize, tex: Option<TexId>) {
        if let Some(s) = self.textures.get_mut(slot) {
            if *s != tex {
                *s = tex;
                self.texture_list = None;
            }
        }
    }

    /// Binds an image for load and store in a slot, or unbinds it.
    pub fn bind_image(&mut self, slot: usize, tex: Option<TexId>) {
        if let Some(s) = self.images.get_mut(slot) {
            if *s != tex {
                *s = tex;
                self.image_list = None;
            }
        }
    }

    /// `nvnCommandBufferClearColor`.
    pub fn clear_color(&mut self, slot: u8) {
        self.stream.cmds.push(Cmd::ClearColor {
            slot,
            full: self.scissor_full,
        });
    }

    /// `nvnCommandBufferClearDepthStencil`.
    pub fn clear_depth_stencil(&mut self) {
        self.stream.cmds.push(Cmd::ClearDepthStencil {
            full: self.scissor_full,
        });
    }

    /// `nvnCommandBufferDiscardColor` and `nvnCommandBufferDiscardDepthStencil`.
    pub fn discard(&mut self, color: u8, depth: bool) {
        self.stream.cmds.push(Cmd::Discard { color, depth });
    }

    fn texture_span(&mut self) -> Span {
        if let Some(s) = self.texture_list {
            return s;
        }
        collect(&self.textures, &mut self.scratch);
        let s = self.stream.push_list(&self.scratch);
        self.texture_list = Some(s);
        s
    }

    /// A draw. The textures that are bound now are its reads.
    pub fn draw(&mut self, state: DrawState) {
        let reads = self.texture_span();
        self.stream.cmds.push(Cmd::Draw { state, reads });
    }

    /// A compute dispatch.
    pub fn dispatch(&mut self) {
        let reads = self.texture_span();
        let writes = match self.image_list {
            Some(s) => s,
            None => {
                collect(&self.images, &mut self.scratch);
                let s = self.stream.push_list(&self.scratch);
                self.image_list = Some(s);
                s
            }
        };
        self.stream.cmds.push(Cmd::Dispatch { reads, writes });
    }

    /// A copy or a blit. A side that is a buffer is None.
    pub fn copy(&mut self, src: Option<TexId>, dst: Option<TexId>) {
        self.stream.cmds.push(Cmd::Copy { src, dst });
    }

    /// `nvnCommandBufferDownsample`.
    pub fn downsample(&mut self, src: TexId, dst: TexId) {
        self.stream.cmds.push(Cmd::Downsample { src, dst });
    }

    /// `nvnCommandBufferBarrier`.
    pub fn barrier(&mut self, bits: u32) {
        self.stream.cmds.push(Cmd::Barrier(bits));
    }

    /// A fence or a wait.
    pub fn sync(&mut self) {
        self.stream.cmds.push(Cmd::Sync);
    }

    /// The game presents a texture.
    pub fn present(&mut self, tex: TexId) {
        self.stream.cmds.push(Cmd::Present(tex));
    }

    /// The CPU reads a texture back.
    pub fn cpu_read(&mut self, tex: TexId) {
        self.stream.cmds.push(Cmd::CpuRead(tex));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_with_the_same_bindings_share_one_list() {
        let mut r = Recorder::new();
        r.bind_texture(0, Some(TexId(5)));
        r.bind_texture(3, Some(TexId(5)));
        r.draw(DrawState::default());
        r.draw(DrawState::default());
        assert_eq!(r.stream().lists, [TexId(5)]);
        r.bind_texture(1, Some(TexId(6)));
        r.draw(DrawState::default());
        assert_eq!(r.stream().lists, [TexId(5), TexId(5), TexId(6)]);
    }

    #[test]
    fn a_scissor_makes_a_clear_partial() {
        let mut r = Recorder::new();
        r.clear_color(0);
        r.set_scissor_full(false);
        r.clear_color(0);
        r.clear_depth_stencil();
        assert_eq!(
            r.stream().cmds[0],
            Cmd::ClearColor {
                slot: 0,
                full: true
            }
        );
        assert_eq!(
            r.stream().cmds[1],
            Cmd::ClearColor {
                slot: 0,
                full: false
            }
        );
        assert_eq!(r.stream().cmds[2], Cmd::ClearDepthStencil { full: false });
    }

    #[test]
    fn begin_forgets_everything_and_keeps_the_memory() {
        let mut r = Recorder::new();
        r.set_render_targets(&[TexId(1)], Some(TexId(2)));
        r.bind_texture(0, Some(TexId(3)));
        r.dispatch();
        let cap = r.stream().cmds.capacity();
        r.begin();
        assert!(r.stream().cmds.is_empty());
        r.dispatch();
        assert!(r.stream().lists.is_empty());
        assert_eq!(r.stream().cmds.capacity(), cap);
    }
}
