//! The native passes as data, shared by every network that `nxkit-gpu` runs natively (`nxkit-ndve`, `nxkit-ndsr`).
//! `nxkit-gpu` turns a [`Plan`] into Metal work; this crate never touches Metal and knows no network. The game's own
//! resources are named by [`GameResource`], whose ids each replacement defines and maps to the bindings of the game's
//! dispatches; the plan's own buffers are named by their index in [`Plan::buffers`].

/// A resource of the game that the replaced dispatches read or wrote. The ids of the variants are the network's own
/// numbering (a network crate gives them names as constants); the replacement in `nxkit-gpu` says where each one is
/// bound in the run, and `nxkit-gpu` resolves it from there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GameResource {
    /// A texture (with its sampler) the replaced dispatches sample or load.
    Texture(u8),
    /// An image the replaced dispatches write, other than the output.
    Image(u8),
    /// The compute push constants of a dispatch (a buffer binding).
    Push(u8),
    /// The image that is the result of the whole replaced run.
    Output,
    /// A picture the game's chain writes besides the output that the game may read later; the plan writes it if it
    /// can, or `nxkit-gpu` keeps the game's own.
    Intermediate {
        /// The network's number of the picture.
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
    /// The kernel's function name in the plan's MSL.
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
