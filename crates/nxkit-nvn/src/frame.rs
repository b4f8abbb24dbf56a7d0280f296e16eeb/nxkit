//! The compact frame format.
//!
//! The front end records every command between two presents into a [`Frame`]. A command is a small `Copy` value.
//! Lists of textures live in one shared `Vec`, and a command points into it with a [`Span`].
//! A [`CmdStream`] is the record of one NVN command buffer. A queue submit appends it to the frame.
//! All vectors keep their capacity from frame to frame, so recording does not allocate once the game is warm.

use std::ops::Range;

use crate::marks::Mark;
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
        let level = if flags & 1 != 0 {
            words[0] >> 16 & 31
        } else {
            0
        };
        let layer = if flags & 2 != 0 { words[1] >> 16 } else { 0 };
        let format = if flags & 4 != 0 {
            (words[1] & 0x1ff) << 21 | 1 << 30
        } else {
            0
        };
        Self(level | layer << 5 | format)
    }
    /// The mip level written by the attachment.
    pub fn level(self) -> u32 {
        self.0 & 31
    }
    /// The array layer or cube face written by the attachment.
    pub fn layer(self) -> u32 {
        self.0 >> 5 & 0xffff
    }
    /// A format override; `None` uses the texture format.
    pub fn format(self) -> Option<i32> {
        (self.0 & (1 << 30) != 0).then_some((self.0 >> 21 & 0x1ff) as i32)
    }
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
    /// The render conditions, reports, waits and signals between the commands, in order (see [`crate::marks`]).
    pub marks: Vec<Mark>,
    /// The indices of the commands that hold a [`Span`], valid when `sealed` equals `cmds.len()` ([`CmdStream::seal`]).
    span_at: Vec<u32>,
    /// The number of commands that `span_at` covers.
    sealed: usize,
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
        self.marks.clear();
        self.span_at.clear();
        self.sealed = 0;
    }

    /// Notes which commands hold a span, so that appending this stream to another copies the commands in one block and
    /// patches only those. A stream that is changed afterwards is appended command by command until it is sealed again.
    pub fn seal(&mut self) {
        self.span_at.clear();
        let at = self
            .cmds
            .iter()
            .enumerate()
            .filter(|(_, c)| has_span(c))
            .map(|(i, _)| i as u32);
        self.span_at.extend(at);
        self.sealed = self.cmds.len();
    }

    /// Appends another stream. Its spans move behind the lists of this stream.
    pub fn splice(&mut self, other: &CmdStream) {
        let base = self.cmds.len() as u32;
        self.marks.extend(other.marks.iter().map(|m| Mark {
            at: m.at + base,
            op: m.op,
        }));
        append(&mut self.cmds, &mut self.lists, other);
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
    /// The ranges of commands that run while a render condition is false, in order and apart. Only the commands that
    /// [`conditional`] names are left out; the others in a range run.
    pub disabled: Vec<Range<u32>>,
}

/// True for a command that a false render condition leaves out: draws, clears, and unless `NXKIT_RENDERCOND=3d`,
/// dispatches, copies and downsamples.
pub fn conditional(c: &Cmd) -> bool {
    match c {
        Cmd::Draw { .. } | Cmd::ClearColor { .. } | Cmd::ClearDepthStencil { .. } => true,
        Cmd::Dispatch { .. } | Cmd::Copy { .. } | Cmd::Downsample { .. } => !only_3d(),
        _ => false,
    }
}

/// `NXKIT_RENDERCOND=3d`: a render condition leaves out draws and clears only.
fn only_3d() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("NXKIT_RENDERCOND").is_ok_and(|v| v == "3d"))
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
        self.disabled.clear();
    }

    /// Marks the commands of `r` as run under a false render condition. The ranges come in command order; one that
    /// touches the last range joins it.
    pub fn disable(&mut self, r: Range<u32>) {
        if r.is_empty() {
            return;
        }
        match self.disabled.last_mut() {
            Some(last) if last.end >= r.start => last.end = last.end.max(r.end),
            _ => self.disabled.push(r),
        }
    }

    /// True when command `i` is left out: it is in a disabled range and [`conditional`].
    pub fn skips(&self, i: u32) -> bool {
        if self.disabled.is_empty() {
            return false;
        }
        let at = self.disabled.partition_point(|r| r.end <= i);
        self.disabled.get(at).is_some_and(|r| r.start <= i)
            && self.cmds.get(i as usize).is_some_and(conditional)
    }

    /// Appends a command buffer. Its spans move to the shared list. The stream stays valid, so the game can submit it again.
    pub fn append(&mut self, s: &CmdStream) {
        append(&mut self.cmds, &mut self.lists, s);
    }

    /// The textures of a span.
    pub fn list(&self, s: Span) -> &[TexId] {
        &self.lists[s.start as usize..(s.start + s.len) as usize]
    }
}

fn has_span(c: &Cmd) -> bool {
    matches!(c, Cmd::Draw { .. } | Cmd::Dispatch { .. })
}

/// Appends the commands and lists of `s`, with the spans moved behind the lists already there. A sealed stream is
/// copied in one block and only its span commands are patched, and not even those when the lists were empty.
fn append(cmds: &mut Vec<Cmd>, lists: &mut Vec<TexId>, s: &CmdStream) {
    let base = lists.len() as u32;
    lists.extend_from_slice(&s.lists);
    if s.sealed != s.cmds.len() {
        cmds.extend(s.cmds.iter().map(|c| rebase(*c, base)));
        return;
    }
    let at = cmds.len();
    cmds.extend_from_slice(&s.cmds);
    if base != 0 {
        for &i in &s.span_at {
            let c = &mut cmds[at + i as usize];
            *c = rebase(*c, base);
        }
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
    fn a_sealed_stream_appends_like_an_unsealed_one() {
        let mut a = CmdStream::new();
        let r = a.push_list(&[t(1), t(2)]);
        let w = a.push_list(&[t(3)]);
        a.cmds.push(Cmd::Barrier(1));
        a.cmds.push(Cmd::Draw {
            state: DrawState::default(),
            reads: r,
        });
        a.cmds.push(Cmd::Dispatch {
            reads: r,
            writes: w,
        });
        a.cmds.push(Cmd::Present(t(9)));
        let mut sealed = CmdStream::new();
        sealed.lists = a.lists.clone();
        sealed.cmds = a.cmds.clone();
        sealed.seal();
        for pre in [0usize, 3] {
            let (mut x, mut y) = (Frame::new(), Frame::new());
            for _ in 0..pre {
                x.lists.push(t(7));
                y.lists.push(t(7));
            }
            x.append(&a);
            x.append(&a);
            y.append(&sealed);
            y.append(&sealed);
            assert_eq!(x.cmds, y.cmds);
            assert_eq!(x.lists, y.lists);
        }
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
        assert_eq!(
            (view.level(), view.layer(), view.format()),
            (2, 5, Some(0x25))
        );
        let identity = TargetView::from_words([0, u32::MAX, u32::MAX, u32::MAX]);
        assert_eq!(identity, TargetView::default());
    }
}
