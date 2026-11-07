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
    /// The engine that runs a service for this game. `None` leaves it to the next layer, and the core default is
    /// [`Runtime::Nxnative`].
    fn runtime(&self, _service: Service) -> Option<Fact<Runtime>> {
        None
    }
    /// Whether the game decodes the shared font with `ScalableFontEngineHelper::Decode` and so expects the BFTTF
    /// header at the font address, not the TTF behind it.
    fn shared_font_decode(&self) -> Option<Fact<bool>> {
        None
    }
    /// The services the game opens through `nn::sf::hipc` that the host answers with zeroed output (service names of
    /// at most 8 bytes). Lists of all layers add up.
    fn sf_services(&self) -> &'static [Fact<&'static str>] {
        &[]
    }
}
