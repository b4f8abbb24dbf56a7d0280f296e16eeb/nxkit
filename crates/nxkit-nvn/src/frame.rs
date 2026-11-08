//! The compact frame format.
//!
//! The front end records every command between two presents into a [`Frame`]. A command is a small `Copy` value.
//! Lists of textures live in one shared `Vec`, and a command points into it with a [`Span`].
//! A [`CmdStream`] is the record of one NVN command buffer. A queue submit appends it to the frame.
//! All vectors keep their capacity from frame to frame, so recording does not allocate once the game is warm.

use crate::resource::TexId;

/// The number of colour attachments that NVN has.
pub const MAX_COLOR: usize = 8;

/// The subresource and optional format of a render attachment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TargetView(u32);

impl TargetView {
    /// Decodes the private 16-byte NVN view layout emitted by the runtime.
    pub fn from_words(words: [u32; 4]) -> Self {
        let flags = words[0] & 0xff;
        let level = if flags & 1 != 0 { words[0] >> 16 & 31 } else { 0 };
        let layer = if flags & 2 != 0 { words[1] >> 16 } else { 0 };
        let format = if flags & 4 != 0 { (words[1] & 0x1ff) << 21 | 1 << 30 } else { 0 };
        Self(level | layer << 5 | format)
    }
    /// The mip level written by the attachment.
    pub fn level(self) -> u32 { self.0 & 31 }
    /// The array layer or cube face written by the attachment.
    pub fn layer(self) -> u32 { self.0 >> 5 & 0xffff }
    /// A format override; `None` uses the texture format.
    pub fn format(self) -> Option<i32> { (self.0 & (1 << 30) != 0).then_some((self.0 >> 21 & 0x1ff) as i32) }
}

/// The attachments that are set at one time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RtSet {
    /// The colour targets by slot.
    pub color: [Option<TexId>; MAX_COLOR],
    /// The depth and stencil target.
    pub depth: Option<TexId>,
    /// The colour views followed by the depth/stencil view at index 8.
    pub views: [TargetView; MAX_COLOR + 1],
}

impl RtSet {
    /// True when no attachment is set.
    pub fn is_empty(&self) -> bool {
        self.depth.is_none() && self.color.iter().all(Option::is_none)
    }
}

/// A run of entries in a texture list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    /// The index of the first entry.
    pub start: u32,
    /// The number of entries.
    pub len: u32,
}

/// The state of a draw that decides what it reads of its own attachments.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct DrawState {
    /// Bit n is set when colour slot n is written.
    pub color_write: u8,
    /// Blending reads the old colour.
    pub blend: bool,
    /// The depth test reads the old depth.
    pub depth_test: bool,
    /// The draw writes depth.
    pub depth_write: bool,
    /// The stencil test reads the old stencil.
    pub stencil_test: bool,
}

/// One recorded command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    /// `nvnCommandBufferSetRenderTargets`.
    SetRenderTargets(RtSet),
    /// A clear of one colour slot. `full` is true when no scissor cuts the target.
    ClearColor {
        /// The colour slot.
        slot: u8,
        /// The clear covers the whole target.
        full: bool,
    },
    /// A clear of depth and stencil.
    ClearDepthStencil {
        /// The clear covers the whole target.
        full: bool,
    },
    /// The game tells that the contents are not needed (`nvnCommandBufferDiscardColor` and the depth variant).
    Discard {
        /// Bit n is set for colour slot n.
        color: u8,
        /// Depth and stencil.
        depth: bool,
    },
    /// A draw with the textures it samples.
    Draw {
        /// What the draw reads of its attachments.
        state: DrawState,
        /// The textures that are bound for sampling.
        reads: Span,
    },
    /// A compute dispatch with the textures it samples and the images it writes.
    Dispatch {
        /// Textures that are read.
        reads: Span,
        /// Images that are written.
        writes: Span,
    },
    /// A copy or blit from `src` to `dst`. A buffer has no texture, so its side is None.
    Copy {
        /// The source texture.
        src: Option<TexId>,
        /// The destination texture.
        dst: Option<TexId>,
    },
    /// A resolve of a multisample texture (`nvnCommandBufferDownsample`).
    Downsample {
        /// The multisample texture.
        src: TexId,
        /// The single sample texture.
        dst: TexId,
    },
    /// `nvnCommandBufferBarrier` with its bits.
    Barrier(u32),
    /// A fence or a wait on the queue.
    Sync,
    /// The game presents a texture.
    Present(TexId),
    /// The CPU reads a texture back.
    CpuRead(TexId),
}

/// The record of one command buffer. The texture lists of its commands are in `lists`.
#[derive(Debug, Default)]
pub struct CmdStream {
    /// The commands in order.
    pub cmds: Vec<Cmd>,
    /// The shared texture lists.
    pub lists: Vec<TexId>,
}

impl CmdStream {
    /// An empty stream.
    pub fn new() -> Self {
        Self::default()
    }

    /// Forgets the commands and keeps the memory.
    pub fn clear(&mut self) {
        self.cmds.clear();
        self.lists.clear();
    }

    /// Appends another stream. Its spans move behind the lists of this stream.
    pub fn splice(&mut self, other: &CmdStream) {
        let base = self.lists.len() as u32;
        self.lists.extend_from_slice(&other.lists);
        self.cmds
            .extend(other.cmds.iter().map(|c| rebase(*c, base)));
    }

    /// Adds a list and returns its span.
    pub fn push_list(&mut self, items: &[TexId]) -> Span {
        let start = self.lists.len() as u32;
        self.lists.extend_from_slice(items);
        Span {
            start,
            len: items.len() as u32,
        }
    }
}

/// All commands between two presents.
#[derive(Clone, Debug, Default)]
pub struct Frame {
    /// The commands in submit order.
    pub cmds: Vec<Cmd>,
    /// The shared texture lists.
    pub lists: Vec<TexId>,
}

impl Frame {
    /// An empty frame.
    pub fn new() -> Self {
        Self::default()
    }

    /// Forgets the commands and keeps the memory.
    pub fn clear(&mut self) {
        self.cmds.clear();
        self.lists.clear();
    }

    /// Appends a command buffer. Its spans move to the shared list. The stream stays valid, so the game can submit it again.
    pub fn append(&mut self, s: &CmdStream) {
        let base = self.lists.len() as u32;
        self.lists.extend_from_slice(&s.lists);
        self.cmds.extend(s.cmds.iter().map(|c| rebase(*c, base)));
    }

    /// The textures of a span.
    pub fn list(&self, s: Span) -> &[TexId] {
        &self.lists[s.start as usize..(s.start + s.len) as usize]
    }
}

fn rebase(c: Cmd, base: u32) -> Cmd {
    let mv = |s: Span| Span {
        start: s.start + base,
        len: s.len,
    };
    match c {
        Cmd::Draw { state, reads } => Cmd::Draw {
            state,
            reads: mv(reads),
        },
        Cmd::Dispatch { reads, writes } => Cmd::Dispatch {
            reads: mv(reads),
            writes: mv(writes),
        },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(n: u32) -> TexId {
        TexId(n)
    }

    #[test]
    fn append_moves_the_spans_and_keeps_the_stream() {
        let mut a = CmdStream::new();
        let s = a.push_list(&[t(1), t(2)]);
        a.cmds.push(Cmd::Draw {
            state: DrawState::default(),
            reads: s,
        });
        let mut f = Frame::new();
        f.append(&a);
        f.append(&a);
        let spans: Vec<Span> = f
            .cmds
            .iter()
            .map(|c| match c {
                Cmd::Draw { reads, .. } => *reads,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(spans[0].start, 0);
        assert_eq!(spans[1].start, 2);
        assert_eq!(f.list(spans[1]), [t(1), t(2)]);
        assert_eq!(a.cmds.len(), 1);
    }

    #[test]
    fn clearing_keeps_the_capacity() {
        let mut f = Frame::new();
        let mut a = CmdStream::new();
        for i in 0..100 {
            a.cmds.push(Cmd::Present(t(i)));
        }
        f.append(&a);
        let cap = f.cmds.capacity();
        f.clear();
        f.append(&a);
        assert_eq!(f.cmds.capacity(), cap);
    }

    #[test]
    fn a_command_is_small() {
        assert_eq!(std::mem::size_of::<TargetView>(), 4);
        assert!(std::mem::size_of::<Cmd>() <= 128);
    }

    #[test]
    fn render_views_decode_mips_layers_and_optional_formats() {
        let view = TargetView::from_words([0x01020007, 0x00050025, 1, 0]);
        assert_eq!((view.level(), view.layer(), view.format()), (2, 5, Some(0x25)));
        let identity = TargetView::from_words([0, u32::MAX, u32::MAX, u32::MAX]);
        assert_eq!(identity, TargetView::default());
    }
}
