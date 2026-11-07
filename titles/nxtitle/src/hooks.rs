//! The questions a family or title answers. Every method has a default of "no opinion".

use crate::{Fact, Runtime, Service};

/// A shader stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShaderStage {
    /// Vertex.
    Vertex,
    /// Tessellation control.
    TessControl,
    /// Tessellation evaluation.
    TessEval,
    /// Geometry.
    Geometry,
    /// Fragment.
    Fragment,
    /// Compute.
    Compute,
    /// Task.
    Task,
    /// Mesh.
    Mesh,
}

/// A shader as the hooks see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShaderView<'a> {
    /// The shader key (16 hex digits).
    pub key: &'a str,
    /// The stage.
    pub stage: ShaderStage,
    /// The path of the shader archive in the game's files, when known.
    pub archive: Option<&'a str>,
}

/// What a shader does for the frame. The core decides how to handle each role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShaderRole {
    /// The game's own upscaler, a pass the host may replace.
    Upscaler {
        /// The name of the upscaler (`agl_ndsr_vr`).
        name: &'static str,
    },
}

/// A shader or pipeline to prepare before the first frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seed {
    /// The shader key (16 hex digits).
    pub key: &'static str,
    /// Where it was seen first (the scene or the run).
    pub seen: &'static str,
}

/// A host behaviour a game depends on, with the setting that turns it on where it is not the default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Need {
    /// The host capability or key (`render_cond`).
    pub capability: &'static str,
    /// The setting the game needs (`on`).
    pub setting: &'static str,
    /// What goes wrong without it.
    pub why: &'static str,
}

/// How the host replaces or keeps the game's upscaler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Upscaler {
    /// The game's own pass runs.
    Game,
    /// The game renders at the output resolution and its upscaler is skipped.
    Native,
    /// MetalFX replaces the game's pass.
    MetalFx,
}

/// Who paces the frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pacing {
    /// The game's own loop.
    Game,
    /// The display's refresh.
    Display,
    /// A fixed rate.
    Fixed {
        /// Frames per second.
        hz: u32,
    },
}

/// What a frame shows when the game sets no cap, as `present_interval` of nxnative's `game.tsv`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentInterval {
    /// A frame stays on screen for the present interval the game set (`game`). A game that sets none gets one tick.
    Game,
    /// A frame stays on screen for one tick of the display, whatever the game set (`display`).
    Display,
}

/// The pacing profile of a game. It carries the keys of nxnative's `game.tsv` that steer the frame step and the
/// presents. Each key has a meaning in `nxkit-svc-pacing` (`LoadConfig`, `PresentSample` and `PresentRule`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaceProfile {
    /// The cap of the frame step in frames per second (`fps_target`): 0, 30, 60 or 120. The step page takes it at
    /// 30 or 60. It is also the target that an engine limiter gets.
    pub fps_target: u32,
    /// Whether a frame that runs late may stay on screen for more vsyncs than the cap says (`fps_hold`). Off for a game
    /// that steps by the cap whatever the hold.
    pub fps_hold: bool,
    /// Whether the present path keeps a ring of two present snapshots (`present_snap`). The lateness of a frame then comes
    /// from what the screen showed.
    pub present_snap: bool,
    /// How long a frame stays on screen when no cap applies (`present_interval`).
    pub present_interval: PresentInterval,
}

/// Questions of the build: shaders, preparation and rewriter rules.
pub trait BuildHooks: Sync {
    /// The role of a shader, when this layer knows it.
    fn shader_role(&self, _shader: &ShaderView<'_>) -> Option<Fact<ShaderRole>> {
        None
    }
    /// Shaders to prepare before the first frame. Lists of all layers add up.
    fn seeds(&self) -> &'static [Seed] {
        &[]
    }
    /// Rewriter rules as TOML text (`rules/*.toml` shape). Rules of all layers add up.
    fn rewrite_rules(&self) -> Option<&'static str> {
        None
    }
}

/// Questions of the host while the game runs.
pub trait RuntimeHooks: Sync {
    /// Host behaviours the game depends on. Lists of all layers add up.
    fn needs(&self) -> &'static [Fact<Need>] {
        &[]
    }
    /// How to handle the game's upscaler.
    fn upscaler(&self) -> Option<Fact<Upscaler>> {
        None
    }
    /// Who paces the frames.
    fn pacing(&self) -> Option<Fact<Pacing>> {
        None
    }
    /// The keys that steer the frame step and the presents. `None` leaves it to the next layer, and the core default is the
    /// plain case: no cap, a hold of the game's interval, no snapshots.
    fn pace_profile(&self) -> Option<Fact<PaceProfile>> {
        None
    }
    /// The engine that runs a service for this game. `None` leaves it to the next layer, and the core default is
    /// [`Runtime::Nxnative`].
    fn runtime(&self, _service: Service) -> Option<Fact<Runtime>> {
        None
    }
}
