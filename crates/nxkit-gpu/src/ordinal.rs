//! The ordinal of a command in a frame: how many draws, clears, copies and dispatches come before it.

use nxkit_nvn::frame::{Cmd, Frame};

/// How many draws, clears, copies and dispatches come before a command.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Ordinal {
    pub(crate) draw: u32,
    pub(crate) clear: u32,
    pub(crate) copy: u32,
    pub(crate) dispatch: u32,
}

/// The ordinals of every command of a frame.
pub(crate) fn ordinals(frame: &Frame) -> Vec<Ordinal> {
    let mut n = Ordinal::default();
    let mut out = Vec::with_capacity(frame.cmds.len());
    for c in &frame.cmds {
        out.push(n);
        match c {
            Cmd::Draw { .. } => n.draw += 1,
            Cmd::ClearColor { .. } | Cmd::ClearDepthStencil { .. } => n.clear += 1,
            Cmd::Copy { .. } | Cmd::BufferCopy | Cmd::Downsample { .. } => n.copy += 1,
            Cmd::Dispatch { .. } => n.dispatch += 1,
            _ => {}
        }
    }
    out
}

/// [`ordinals`] with one more entry: how many draws, clears, copies and dispatches the whole frame has, so that
/// `[ords[a], ords[b])` is what the commands `a..b` hold for every `b` up to the number of commands.
pub(crate) fn ordinals_with_end(frame: &Frame) -> Vec<Ordinal> {
    let mut out = ordinals(frame);
    let mut n = out.last().copied().unwrap_or_default();
    if let Some(c) = frame.cmds.last() {
        match c {
            Cmd::Draw { .. } => n.draw += 1,
            Cmd::ClearColor { .. } | Cmd::ClearDepthStencil { .. } => n.clear += 1,
            Cmd::Copy { .. } | Cmd::BufferCopy | Cmd::Downsample { .. } => n.copy += 1,
            Cmd::Dispatch { .. } => n.dispatch += 1,
            _ => {}
        }
    }
    out.push(n);
    out
}
