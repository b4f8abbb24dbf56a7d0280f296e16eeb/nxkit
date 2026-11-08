//! The native passes as data. `nxkit-gpu` turns a [`Plan`] into Metal work; this crate never touches Metal.
//!
//! A plan replaces the game's dispatches from the colour conversion to the resampler (passes 3 to 16 of
//! `docs/plans/ndve-native.md`, section 4) with the dispatches of [`Plan::steps`]. The game's own resources are named
//! by [`GameResource`]; the plan's own buffers by their index in [`Plan::buffers`].

/// A resource of the game that the replaced chain read or wrote. `nxkit-gpu` resolves each from the bindings of the
/// game's dispatches it replaces (the index of that dispatch in the matched run, counted from 0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GameResource {
    /// The Y plane the colour conversion reads (R8, 1920x1080).
    LumaPlane,
    /// The CbCr plane the colour conversion samples (RG8, 960x540), with the game's sampler.
    ChromaPlane,
    /// The image the resampler writes (RGBA8, 2560x1440): the output of the whole chain.
    Output,
    /// The dither texture the resampler reads (`& 0x1ff` coordinates).
    Dither,
    /// The push constants of the colour conversion (bank 24, 60 bytes).
    ConversionPush,
    /// The push constants of the resampler (bank 24, 84 bytes).
    ResamplerPush,
    /// The images the game's chain writes besides the output that the game may read later: the RGBA8 picture of the
    /// conversion and the RGB10A2 picture of layer 12.
    Intermediate {
        /// 0 is the RGBA8 picture, 1 the RGB10A2 one.
        index: u8,
    },
}

/// A buffer the plan owns. `nxkit-gpu` allocates it once per plan and keeps it while the plan is in use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Buffer {
    /// For labels and diagnostics.
    pub name: &'static str,
    /// The size in bytes.
    pub bytes: u64,
    /// The bytes to upload once when the buffer is made (the packed weights), or `None` for scratch.
    pub init: Option<Vec<u8>>,
}

/// What one binding of a step is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// A buffer of [`Plan::buffers`].
    Buffer(usize),
    /// A resource of the game.
    Game(GameResource),
}

/// One binding: the Metal argument index in the kernel and what is bound there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    /// The `[[buffer(n)]]` or `[[texture(n)]]` index in the kernel.
    pub index: u32,
    /// What is bound.
    pub source: Source,
    /// The kernel writes it.
    pub writes: bool,
}

/// One dispatch of the plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    /// The kernel's function name in [`crate::msl::SOURCE`].
    pub kernel: &'static str,
    /// The threadgroups.
    pub groups: [u32; 3],
    /// The threads per threadgroup.
    pub threads: [u32; 3],
    /// The threadgroup memory in bytes, at `[[threadgroup(0)]]`, or 0.
    pub threadgroup_bytes: u32,
    /// The buffer bindings.
    pub buffers: Vec<Binding>,
    /// The texture bindings.
    pub textures: Vec<Binding>,
}

/// The native passes of one network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The buffers the plan owns: packed weights, activations, skips.
    pub buffers: Vec<Buffer>,
    /// The dispatches, in order. Each step reads what the steps before it wrote; `nxkit-gpu` puts a barrier between
    /// steps that the bindings show depend on each other.
    pub steps: Vec<Step>,
}
