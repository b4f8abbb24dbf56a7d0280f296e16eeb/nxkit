//! The list of NVN functions.
//!
//! The crate carries two lists as data.
//! - [`NVN_NAMES`] is every NVN function name from nxnative (`dm/gfx/nvn/names.txt`).
//! - [`NXNATIVE_IMPLEMENTED`] is the part that nxnative implements in `hle_nvn.c`.
//!
//! [`RECORDED`] is the part that this front end handles. Each entry names the [`Op`] that a call turns into.
//! [`FFI_GROUPS`] is the part that the C ABI of `nxkit-rt` implements without the front end recording anything,
//! one row per group (device, queue, memory pool and so on).
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
    /// Starts a recording.
    BeginRecording,
    /// Ends a recording and gives its handle.
    EndRecording,
    /// Gives a command buffer a block of command memory.
    AddCommandMemory,
    /// Sets the memory callback, its data or its enabled flag.
    SetMemoryCallback,
    /// Asks how much command memory is used, free or in total.
    CommandMemoryInfo,
    /// The CPU waits for the GPU. This flushes the frame early.
    CpuWait,
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
    ("nvnCommandBufferEndRecording", Op::EndRecording),
    ("nvnCommandBufferAddCommandMemory", Op::AddCommandMemory),
    (
        "nvnCommandBufferGetCommandMemoryUsed",
        Op::CommandMemoryInfo,
    ),
    (
        "nvnCommandBufferGetCommandMemoryFree",
        Op::CommandMemoryInfo,
    ),
    (
        "nvnCommandBufferGetCommandMemorySize",
        Op::CommandMemoryInfo,
    ),
    ("nvnCommandBufferSetMemoryCallback", Op::SetMemoryCallback),
    (
        "nvnCommandBufferSetMemoryCallbackData",
        Op::SetMemoryCallback,
    ),
    (
        "nvnCommandBufferSetCommandMemoryCallbackEnabled",
        Op::SetMemoryCallback,
    ),
    ("nvnQueueFinish", Op::CpuWait),
    ("nvnSyncWait", Op::CpuWait),
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

/// The functions that the C ABI of `nxkit-rt` implements, in one row per group. A group module of `nxkit-rt` owns its row.
/// A test of `nxkit-rt` checks that the names of a row are the names that the group exports.
/// A function that is in [`RECORDED`] and in a row is counted once.
pub const FFI_GROUPS: &[(&str, &[&str])] = &[
    (
        "device",
        &[
            "nvnDeviceBuilderSetDefaults",
            "nvnDeviceBuilderSetFlags",
            "nvnDeviceInitialize",
            "nvnDeviceFinalize",
            "nvnDeviceFinalizeCommandHandle",
            "nvnDeviceGetProcAddress",
            "nvnDeviceGetInteger",
            "nvnDeviceGetCurrentTimestampInNanoseconds",
            "nvnDeviceGetTimestampInNanoseconds",
            "nvnDeviceGetTextureHandle",
            "nvnDeviceGetTexelFetchHandle",
            "nvnDeviceGetImageHandle",
            "nvnDeviceGetSeparateTextureHandle",
            "nvnDeviceGetSeparateSamplerHandle",
            "nvnDeviceInstallDebugCallback",
            "nvnDeviceSetWindowOriginMode",
            "nvnDeviceGetWindowOriginMode",
            "nvnDeviceSetDepthMode",
            "nvnDeviceGetDepthMode",
            "nvnDeviceRegisterFastClearColor",
            "nvnDeviceRegisterFastClearDepth",
            "nvnDeviceSetIntermediateShaderCache",
            "nvnSetExternalDebuggerEnabledNVX",
        ],
    ),
    (
        "queue",
        &[
            "nvnQueueBuilderSetDevice",
            "nvnQueueBuilderSetDefaults",
            "nvnQueueBuilderSetFlags",
            "nvnQueueBuilderSetCommandMemorySize",
            "nvnQueueBuilderSetComputeMemorySize",
            "nvnQueueBuilderSetControlMemorySize",
            "nvnQueueBuilderSetCommandFlushThreshold",
            "nvnQueueBuilderSetQueueMemory",
            "nvnQueueBuilderSetQueuePriority",
            "nvnQueueBuilderSetPrimaryQueue",
            "nvnQueueBuilderGetQueueMemorySize",
            "nvnQueueInitialize",
            "nvnQueueFinalize",
            "nvnQueueSetDebugLabel",
            "nvnQueueSubmitCommands",
            "nvnQueueFlush",
            "nvnQueueFinish",
            "nvnQueuePresentTexture",
            "nvnQueueAcquireTexture",
            "nvnQueueFenceSync",
            "nvnQueueWaitSync",
            "nvnQueueGetError",
        ],
    ),
    (
        "sync",
        &[
            "nvnSyncInitialize",
            "nvnSyncFinalize",
            "nvnSyncSetDebugLabel",
            "nvnSyncWait",
        ],
    ),
    (
        "pool",
        &[
            "nvnMemoryPoolBuilderSetDevice",
            "nvnMemoryPoolBuilderSetDefaults",
            "nvnMemoryPoolBuilderSetStorage",
            "nvnMemoryPoolBuilderSetFlags",
            "nvnMemoryPoolInitialize",
            "nvnMemoryPoolFinalize",
            "nvnMemoryPoolSetDebugLabel",
            "nvnMemoryPoolMap",
            "nvnMemoryPoolFlushMappedRange",
            "nvnMemoryPoolInvalidateMappedRange",
            "nvnMemoryPoolGetBufferAddress",
            "nvnMemoryPoolMapVirtual",
            "nvnMemoryPoolGetSize",
            "nvnMemoryPoolGetFlags",
        ],
    ),
    (
        "buffer",
        &[
            "nvnBufferBuilderSetDevice",
            "nvnBufferBuilderSetDefaults",
            "nvnBufferBuilderSetStorage",
            "nvnBufferBuilderGetMemoryPool",
            "nvnBufferBuilderGetMemoryOffset",
            "nvnBufferBuilderGetSize",
            "nvnBufferInitialize",
            "nvnBufferFinalize",
            "nvnBufferSetDebugLabel",
            "nvnBufferMap",
            "nvnBufferGetAddress",
            "nvnBufferFlushMappedRange",
            "nvnBufferInvalidateMappedRange",
            "nvnBufferGetMemoryPool",
            "nvnBufferGetMemoryOffset",
            "nvnBufferGetSize",
        ],
    ),
    (
        "window",
        &[
            "nvnWindowBuilderSetDevice",
            "nvnWindowBuilderSetDefaults",
            "nvnWindowBuilderSetNativeWindow",
            "nvnWindowBuilderSetTextures",
            "nvnWindowBuilderSetNumActiveTextures",
            "nvnWindowBuilderSetPresentInterval",
            "nvnWindowBuilderGetPresentInterval",
            "nvnWindowInitialize",
            "nvnWindowSetDebugLabel",
            "nvnWindowAcquireTexture",
            "nvnWindowSetNumActiveTextures",
            "nvnWindowSetPresentInterval",
            "nvnWindowGetPresentInterval",
            "nvnWindowSetCrop",
            "nvnWindowGetCrop",
        ],
    ),
    (
        "event",
        &[
            "nvnEventBuilderSetDefaults",
            "nvnEventBuilderSetStorage",
            "nvnEventBuilderGetMemoryPool",
            "nvnEventBuilderGetMemoryOffset",
            "nvnEventInitialize",
            "nvnEventFinalize",
            "nvnEventGetValue",
            "nvnEventSignal",
            "nvnEventGetMemoryPool",
            "nvnEventGetMemoryOffset",
        ],
    ),
    (
        "program",
        &[
            "nvnProgramBuilderSetDevice",
            "nvnProgramBuilderSetDefaults",
            "nvnProgramBuilderSetShaders",
            "nvnProgramInitialize",
            "nvnProgramInitializeFromBuilder",
            "nvnProgramFinalize",
            "nvnProgramSetDebugLabel",
            "nvnProgramSetShaders",
            "nvnProgramSetShadersExt",
            "nvnProgramSetSampleShading",
        ],
    ),
    (
        "cmdbuf",
        &[
            "nvnCommandBufferAddCommandMemory",
            "nvnCommandBufferAddControlMemory",
            "nvnCommandBufferBarrier",
            "nvnCommandBufferBeginRecording",
            "nvnCommandBufferBeginTransformFeedback",
            "nvnCommandBufferBindBlendState",
            "nvnCommandBufferBindChannelMaskState",
            "nvnCommandBufferBindColorState",
            "nvnCommandBufferBindDepthStencilState",
            "nvnCommandBufferBindImage",
            "nvnCommandBufferBindImages",
            "nvnCommandBufferBindMultisampleState",
            "nvnCommandBufferBindPolygonState",
            "nvnCommandBufferBindProgram",
            "nvnCommandBufferBindSeparateSampler",
            "nvnCommandBufferBindSeparateSamplers",
            "nvnCommandBufferBindSeparateTexture",
            "nvnCommandBufferBindSeparateTextures",
            "nvnCommandBufferBindShadingRateImage",
            "nvnCommandBufferBindStorageBuffer",
            "nvnCommandBufferBindStorageBuffers",
            "nvnCommandBufferBindTexture",
            "nvnCommandBufferBindTextures",
            "nvnCommandBufferBindTexturesDeferred",
            "nvnCommandBufferBindTransformFeedbackBuffer",
            "nvnCommandBufferBindTransformFeedbackBuffers",
            "nvnCommandBufferBindUniformBuffer",
            "nvnCommandBufferBindUniformBuffers",
            "nvnCommandBufferBindVertexAttribState",
            "nvnCommandBufferBindVertexBuffer",
            "nvnCommandBufferBindVertexBuffers",
            "nvnCommandBufferBindVertexStreamState",
            "nvnCommandBufferCallCommands",
            "nvnCommandBufferClearBuffer",
            "nvnCommandBufferClearColor",
            "nvnCommandBufferClearColori",
            "nvnCommandBufferClearColorui",
            "nvnCommandBufferClearDepthStencil",
            "nvnCommandBufferClearTexture",
            "nvnCommandBufferClearTextureui",
            "nvnCommandBufferCopyBufferToBuffer",
            "nvnCommandBufferCopyBufferToTexture",
            "nvnCommandBufferCopyCommands",
            "nvnCommandBufferCopyTextureToBuffer",
            "nvnCommandBufferCopyTextureToTexture",
            "nvnCommandBufferDiscardColor",
            "nvnCommandBufferDiscardDepthStencil",
            "nvnCommandBufferDispatchCompute",
            "nvnCommandBufferDispatchComputeIndirect",
            "nvnCommandBufferDownsample",
            "nvnCommandBufferDownsampleTextureView",
            "nvnCommandBufferDrawArrays",
            "nvnCommandBufferDrawArraysIndirect",
            "nvnCommandBufferDrawArraysInstanced",
            "nvnCommandBufferDrawElements",
            "nvnCommandBufferDrawElementsBaseVertex",
            "nvnCommandBufferDrawElementsIndirect",
            "nvnCommandBufferDrawElementsInstanced",
            "nvnCommandBufferDrawMeshTasks",
            "nvnCommandBufferDrawMeshTasksIndirect",
            "nvnCommandBufferDrawTexture",
            "nvnCommandBufferDrawTransformFeedback",
            "nvnCommandBufferEndRecording",
            "nvnCommandBufferEndTransformFeedback",
            "nvnCommandBufferFenceSync",
            "nvnCommandBufferFinalize",
            "nvnCommandBufferGetCommandMemoryFree",
            "nvnCommandBufferGetCommandMemorySize",
            "nvnCommandBufferGetCommandMemoryUsed",
            "nvnCommandBufferGetControlMemoryFree",
            "nvnCommandBufferGetControlMemorySize",
            "nvnCommandBufferGetControlMemoryUsed",
            "nvnCommandBufferGetCopyImageStride",
            "nvnCommandBufferGetCopyRowStride",
            "nvnCommandBufferInitialize",
            "nvnCommandBufferInsertDebugMarker",
            "nvnCommandBufferInsertDebugMarkerDynamic",
            "nvnCommandBufferInsertDebugMarkerStatic",
            "nvnCommandBufferIsRecording",
            "nvnCommandBufferMultiDrawArraysIndirectCount",
            "nvnCommandBufferMultiDrawElementsIndirectCount",
            "nvnCommandBufferPauseTransformFeedback",
            "nvnCommandBufferPopDebugGroup",
            "nvnCommandBufferPopDebugGroupId",
            "nvnCommandBufferPushComputeConstants",
            "nvnCommandBufferPushDebugGroup",
            "nvnCommandBufferPushDebugGroupDynamic",
            "nvnCommandBufferPushDebugGroupStatic",
            "nvnCommandBufferPushGraphicsConstants",
            "nvnCommandBufferReportCounter",
            "nvnCommandBufferReportValue",
            "nvnCommandBufferResetCounter",
            "nvnCommandBufferRestoreZCullData",
            "nvnCommandBufferResumeTransformFeedback",
            "nvnCommandBufferSaveZCullData",
            "nvnCommandBufferSetAlphaRef",
            "nvnCommandBufferSetBlendColor",
            "nvnCommandBufferSetColorReductionEnable",
            "nvnCommandBufferSetColorReductionThresholds",
            "nvnCommandBufferSetCommandMemoryCallbackEnabled",
            "nvnCommandBufferSetComputeOverlapMode",
            "nvnCommandBufferSetComputeWorkloadPriority",
            "nvnCommandBufferSetConservativeRasterEnable",
            "nvnCommandBufferSetCopyImageStride",
            "nvnCommandBufferSetCopyRowStride",
            "nvnCommandBufferSetDebugLabel",
            "nvnCommandBufferSetDepthBounds",
            "nvnCommandBufferSetDepthClamp",
            "nvnCommandBufferSetDepthRange",
            "nvnCommandBufferSetDepthRanges",
            "nvnCommandBufferSetGeometryProcessingMode",
            "nvnCommandBufferSetLineWidth",
            "nvnCommandBufferSetMemoryCallback",
            "nvnCommandBufferSetMemoryCallbackData",
            "nvnCommandBufferSetPatchSize",
            "nvnCommandBufferSetPointSize",
            "nvnCommandBufferSetPolygonOffsetClamp",
            "nvnCommandBufferSetPrimitiveRestart",
            "nvnCommandBufferSetRasterizerDiscard",
            "nvnCommandBufferSetRecordingMode",
            "nvnCommandBufferSetRenderEnable",
            "nvnCommandBufferSetRenderEnableConditional",
            "nvnCommandBufferSetRenderTargets",
            "nvnCommandBufferSetSampleMask",
            "nvnCommandBufferSetSamplerPool",
            "nvnCommandBufferSetScissor",
            "nvnCommandBufferSetScissors",
            "nvnCommandBufferSetShaderScratchMemory",
            "nvnCommandBufferSetShadingRatePaletteMode",
            "nvnCommandBufferSetStencilCullCriteria",
            "nvnCommandBufferSetStencilMask",
            "nvnCommandBufferSetStencilRef",
            "nvnCommandBufferSetStencilValueMask",
            "nvnCommandBufferSetTexturePool",
            "nvnCommandBufferSetTiledCacheAction",
            "nvnCommandBufferSetTiledCacheTileSize",
            "nvnCommandBufferSetViewport",
            "nvnCommandBufferSetViewportSwizzles",
            "nvnCommandBufferSetViewports",
            "nvnCommandBufferSetZCullZF32CompressionEnable",
            "nvnCommandBufferShadingRateImagePalette",
            "nvnCommandBufferSignalEvent",
            "nvnCommandBufferUpdateUniformBuffer",
            "nvnCommandBufferWaitEvent",
            "nvnCommandBufferWaitSync",
        ],
    ),
];

/// The functions that nxnative answers with an empty body (`g_nops` in `hle_nvn.c`). They are not in
/// [`NXNATIVE_IMPLEMENTED`]. A group of `nxkit-rt` may answer them and list them in [`FFI_GROUPS`].
pub const NXNATIVE_EMPTY: &[&str] = &[
    "nvnCommandBufferSetPatchSize",
    "nvnCommandBufferSetDepthBounds",
    "nvnCommandBufferWaitSync",
    "nvnCommandBufferSetShaderScratchMemory",
    "nvnCommandBufferPushDebugGroupDynamic",
    "nvnCommandBufferPopDebugGroup",
    "nvnCommandBufferInsertDebugMarkerDynamic",
    "nvnCommandBufferInsertDebugMarker",
    "nvnCommandBufferPushDebugGroup",
    "nvnCommandBufferPushDebugGroupStatic",
    "nvnCommandBufferPopDebugGroupId",
    "nvnCommandBufferInsertDebugMarkerStatic",
    "nvnCommandBufferWaitEvent",
];

/// Whether a group of `nxkit-rt` implements the function.
pub fn in_ffi(name: &str) -> bool {
    FFI_GROUPS.iter().any(|(_, names)| names.contains(&name))
}

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
            let bucket = if op_of(name).is_some() || in_ffi(name) {
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
    fn every_ffi_name_is_implemented_by_nxnative_and_listed_once() {
        let known: HashSet<&str> = parse_names(NXNATIVE_IMPLEMENTED).collect();
        let mut seen = HashSet::new();
        for (group, names) in FFI_GROUPS {
            for name in *names {
                assert!(
                    known.contains(name) || NXNATIVE_EMPTY.contains(name),
                    "{group}: {name} has no nxnative reference"
                );
                assert!(seen.insert(*name), "{name} is in two groups or twice");
            }
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
        assert_eq!(
            c.handled,
            ["nvnCommandBufferBarrier", "nvnDeviceInitialize"]
        );
        assert!(c.unhandled.is_empty());
        assert_eq!(c.unknown, ["nvnNotAnNvnName"]);
    }

    #[test]
    fn the_whole_nxnative_list_splits_without_loss() {
        let c = Coverage::of_nxnative();
        let known: HashSet<&str> = parse_names(NXNATIVE_IMPLEMENTED).collect();
        let ffi_only = FFI_GROUPS
            .iter()
            .flat_map(|(_, names)| names.iter())
            .filter(|n| op_of(n).is_none() && known.contains(*n))
            .count();
        assert_eq!(c.handled.len(), RECORDED.len() + ffi_only);
        assert_eq!(c.handled.len() + c.unhandled.len(), 484);
        assert!(c.unknown.is_empty());
    }
}
