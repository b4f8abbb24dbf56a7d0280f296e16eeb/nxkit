//! The facts behind `settings.temporal_upscale` of `game.json` (R72). The engine knows how its view uniform buffer and
//! its velocity texture are laid out ([`TemporalView`]). The title knows which draws of its frame to replace
//! ([`TemporalDraws`]). `nxb build` writes the key when both are answered.

/// A draw by the shader key of its fragment program and one of its fragment texture slots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DrawSlot {
    /// The shader key of the fragment program (16 hex digits).
    pub fragment: &'static str,
    /// The fragment texture slot.
    pub slot: u32,
}

/// The stage of a uniform buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UboStage {
    /// The vertex stage.
    Vertex,
    /// The fragment stage.
    Fragment,
}

impl UboStage {
    /// The name in `game.json`.
    pub const fn id(self) -> &'static str {
        match self {
            UboStage::Vertex => "vertex",
            UboStage::Fragment => "fragment",
        }
    }
}

/// The draw that gives the depth, the velocity and the view uniform buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceDraw {
    /// The shader key of the fragment program (16 hex digits).
    pub fragment: &'static str,
    /// The fragment texture slot of the depth.
    pub depth_slot: u32,
    /// The fragment texture slot of the velocity.
    pub velocity_slot: u32,
    /// The stage of the view uniform buffer.
    pub view_stage: UboStage,
    /// The slot of the view uniform buffer.
    pub view_slot: u32,
}

/// The draws of one title that the temporal upscaler replaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TemporalDraws {
    /// The upscale draw and the slot of its jittered picture.
    pub upscale: DrawSlot,
    /// The draw whose bindings give the depth, the velocity and the view uniform buffer.
    pub source: SourceDraw,
    /// The draws that become a copy of the texture at their slot.
    pub passthrough: &'static [DrawSlot],
}

/// How the engine writes the velocity: a texel `raw` is the velocity `raw * scale + bias` in clip units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VelocityCode {
    /// The factor of x and y.
    pub scale: [f64; 2],
    /// The offset of x and y.
    pub bias: [f64; 2],
    /// Whether y points up.
    pub y_up: bool,
    /// Whether a texel of 0 in both channels means that no velocity was written, so the camera motion stands in.
    pub zero_means_camera: bool,
}

/// What the engine's layout says about the inputs of the temporal upscaler.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TemporalView {
    /// The byte offset in the view uniform buffer of the clip to previous clip matrix (16 floats, by rows).
    pub clip_to_prev_clip: u32,
    /// The byte offset of the jitter of this frame in clip units (2 floats).
    pub jitter: u32,
    /// The byte offset of the view size in pixels (2 floats).
    pub view_size: u32,
    /// The velocity code.
    pub velocity: VelocityCode,
    /// Whether the depth is 1 at the near plane.
    pub depth_reversed: bool,
}
