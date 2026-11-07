//! The list of NVN functions.
//!
//! The crate carries two lists as data.
//! - [`NVN_NAMES`] is every NVN function name from nxnative (`dm/gfx/nvn/names.txt`).
//! - [`NXNATIVE_IMPLEMENTED`] is the part that nxnative implements in `hle_nvn.c`.
//!
//! [`RECORDED`] is the part that this front end handles. Each entry names the [`Op`] that a call turns into.
//! The GPU moves to nxkit one game at a time. [`Coverage::of`] takes the names that one game imports
//! and reports the ones that this front end does not handle yet.

use std::collections::HashSet;

/// Every NVN function name that nxnative knows.
pub const NVN_NAMES: &str = include_str!("../data/nvn_names.txt");
/// The NVN functions that nxnative implements.
pub const NXNATIVE_IMPLEMENTED: &str = include_str!("../data/nxnative_implemented.txt");

/// What the front end does for a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Op {
    /// Starts a command buffer.
    BeginRecording,
    /// Sets the attachments.
    SetRenderTargets,
    /// Sets the scissor.
    SetScissor,
    /// Clears a colour target.
    ClearColor,
    /// Clears depth and stencil.
    ClearDepthStencil,
    /// Tells that contents are not needed.
    Discard,
    /// Sets a draw state that decides what a draw reads.
    BindState,
    /// Binds textures for sampling.
    BindTexture,
    /// Binds images for load and store.
    BindImage,
    /// Draws.
    Draw,
    /// Runs compute.
    Dispatch,
    /// Copies, blits and clears of textures and buffers.
    Copy,
    /// Resolves multisample textures.
    Downsample,
    /// A barrier.
    Barrier,
    /// A fence or a wait.
    Sync,
    /// Plays a recorded command list.
    CallCommands,
    /// Submits command buffers to the queue.
    Submit,
    /// Presents a texture.
    Present,
    /// Builds a texture handle.
    TextureHandle,
    /// Creates a texture or sampler pool.
    TexturePool,
    /// Registers a texture or image in a pool.
    RegisterTexture,
    /// Registers a sampler in a pool.
    RegisterSampler,
    /// Creates a texture.
    TextureInit,
    /// Destroys a texture.
    TextureFinalize,
    /// Gives a GPU address.
    TextureAddress,
    /// The CPU reads texels back.
    TextureRead,
}

/// The functions that the front end handles.
pub const RECORDED: &[(&str, Op)] = &[
    ("nvnCommandBufferBeginRecording", Op::BeginRecording),
    ("nvnCommandBufferSetRenderTargets", Op::SetRenderTargets),
    ("nvnCommandBufferSetScissor", Op::SetScissor),
    ("nvnCommandBufferSetScissors", Op::SetScissor),
    ("nvnCommandBufferClearColor", Op::ClearColor),
    ("nvnCommandBufferClearColori", Op::ClearColor),
    ("nvnCommandBufferClearColorui", Op::ClearColor),
    ("nvnCommandBufferClearDepthStencil", Op::ClearDepthStencil),
    ("nvnCommandBufferDiscardColor", Op::Discard),
    ("nvnCommandBufferDiscardDepthStencil", Op::Discard),
    ("nvnCommandBufferBindBlendState", Op::BindState),
    ("nvnCommandBufferBindChannelMaskState", Op::BindState),
    ("nvnCommandBufferBindColorState", Op::BindState),
    ("nvnCommandBufferBindDepthStencilState", Op::BindState),
    ("nvnCommandBufferBindTexture", Op::BindTexture),
    ("nvnCommandBufferBindTextures", Op::BindTexture),
    ("nvnCommandBufferBindImage", Op::BindImage),
    ("nvnCommandBufferBindImages", Op::BindImage),
    ("nvnCommandBufferDrawArrays", Op::Draw),
    ("nvnCommandBufferDrawElements", Op::Draw),
    ("nvnCommandBufferDrawElementsBaseVertex", Op::Draw),
    ("nvnCommandBufferDrawArraysInstanced", Op::Draw),
    ("nvnCommandBufferDrawElementsInstanced", Op::Draw),
    ("nvnCommandBufferDrawArraysIndirect", Op::Draw),
    ("nvnCommandBufferDrawElementsIndirect", Op::Draw),
    ("nvnCommandBufferDrawTransformFeedback", Op::Draw),
    ("nvnCommandBufferDrawTexture", Op::Draw),
    ("nvnCommandBufferDispatchCompute", Op::Dispatch),
    ("nvnCommandBufferDispatchComputeIndirect", Op::Dispatch),
    ("nvnCommandBufferCopyBufferToTexture", Op::Copy),
    ("nvnCommandBufferCopyTextureToBuffer", Op::Copy),
    ("nvnCommandBufferCopyTextureToTexture", Op::Copy),
    ("nvnCommandBufferCopyBufferToBuffer", Op::Copy),
    ("nvnCommandBufferClearTexture", Op::Copy),
    ("nvnCommandBufferClearTextureui", Op::Copy),
    ("nvnCommandBufferClearBuffer", Op::Copy),
    ("nvnCommandBufferDownsample", Op::Downsample),
    ("nvnCommandBufferDownsampleTextureView", Op::Downsample),
    ("nvnCommandBufferBarrier", Op::Barrier),
    ("nvnCommandBufferFenceSync", Op::Sync),
    ("nvnQueueFenceSync", Op::Sync),
    ("nvnQueueWaitSync", Op::Sync),
    ("nvnCommandBufferCallCommands", Op::CallCommands),
    ("nvnQueueSubmitCommands", Op::Submit),
    ("nvnQueuePresentTexture", Op::Present),
    ("nvnDeviceGetTextureHandle", Op::TextureHandle),
    ("nvnTexturePoolInitialize", Op::TexturePool),
    ("nvnSamplerPoolInitialize", Op::TexturePool),
    ("nvnTexturePoolRegisterTexture", Op::RegisterTexture),
    ("nvnTexturePoolRegisterImage", Op::RegisterTexture),
    ("nvnSamplerPoolRegisterSampler", Op::RegisterSampler),
    ("nvnTextureInitialize", Op::TextureInit),
    ("nvnTextureFinalize", Op::TextureFinalize),
    ("nvnTextureGetTextureAddress", Op::TextureAddress),
    ("nvnMemoryPoolGetBufferAddress", Op::TextureAddress),
    ("nvnTextureReadTexels", Op::TextureRead),
    ("nvnTextureReadTexelsStrided", Op::TextureRead),
];

/// The names of a list, one per line. Empty lines and lines that start with `#` are skipped.
pub fn parse_names(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
}

/// The op of a function, or None when the front end does not handle it.
pub fn op_of(name: &str) -> Option<Op> {
    RECORDED.iter().find(|(n, _)| *n == name).map(|(_, op)| *op)
}

/// How a list of function names splits against this front end.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Coverage {
    /// Names that the front end records.
    pub handled: Vec<String>,
    /// Names that nxnative implements and the front end does not handle yet.
    pub unhandled: Vec<String>,
    /// Names that nxnative does not implement. A game that calls them has no reference to port from.
    pub unknown: Vec<String>,
}

impl Coverage {
    /// Splits the names of `list`.
    pub fn of<'a>(list: impl IntoIterator<Item = &'a str>) -> Self {
        let known: HashSet<&str> = parse_names(NXNATIVE_IMPLEMENTED).collect();
        let mut c = Self::default();
        let mut seen = HashSet::new();
        for name in list {
            if !seen.insert(name) {
                continue;
            }
            let bucket = if op_of(name).is_some() {
                &mut c.handled
            } else if known.contains(name) {
                &mut c.unhandled
            } else {
                &mut c.unknown
            };
            bucket.push(name.to_string());
        }
        c
    }

    /// The coverage of everything that nxnative implements.
    pub fn of_nxnative() -> Self {
        Self::of(parse_names(NXNATIVE_IMPLEMENTED))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lists_have_the_sizes_of_nxnative() {
        assert_eq!(parse_names(NVN_NAMES).count(), 743);
        assert_eq!(parse_names(NXNATIVE_IMPLEMENTED).count(), 484);
    }

    #[test]
    fn every_implemented_name_is_a_known_name() {
        let all: HashSet<&str> = parse_names(NVN_NAMES).collect();
        assert!(parse_names(NXNATIVE_IMPLEMENTED).all(|n| all.contains(n)));
    }

    #[test]
    fn every_recorded_name_is_known_and_listed_once() {
        let all: HashSet<&str> = parse_names(NVN_NAMES).collect();
        let mut seen = HashSet::new();
        for (name, _) in RECORDED {
            assert!(all.contains(name), "{name} is not an NVN name");
            assert!(seen.insert(*name), "{name} is listed twice");
        }
    }

    #[test]
    fn every_recorded_name_is_implemented_by_nxnative() {
        let known: HashSet<&str> = parse_names(NXNATIVE_IMPLEMENTED).collect();
        for (name, _) in RECORDED {
            assert!(known.contains(name), "{name} has no nxnative reference");
        }
    }

    #[test]
    fn coverage_splits_a_list_in_three() {
        let c = Coverage::of([
            "nvnCommandBufferBarrier",
            "nvnCommandBufferBarrier",
            "nvnDeviceInitialize",
            "nvnNotAnNvnName",
        ]);
        assert_eq!(c.handled, ["nvnCommandBufferBarrier"]);
        assert_eq!(c.unhandled, ["nvnDeviceInitialize"]);
        assert_eq!(c.unknown, ["nvnNotAnNvnName"]);
    }

    #[test]
    fn the_whole_nxnative_list_splits_without_loss() {
        let c = Coverage::of_nxnative();
        assert_eq!(c.handled.len(), RECORDED.len());
        assert_eq!(c.handled.len() + c.unhandled.len(), 484);
        assert!(c.unknown.is_empty());
    }
}
