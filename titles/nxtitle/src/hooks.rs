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

/// The data `nxb build` needs to make a game without nxnative: no engine folder, no Python and no `dmhost`.
#[derive(Clone, Copy, Debug)]
pub struct Standalone {
    /// The packed version the data was made for (`1114112` for v1114112). A build of another version is refused.
    pub version: u32,
    /// The game's modules in load order (`main`, `subsdk0`, `sdk`), each with the build ID of that version (the
    /// first 16 bytes of the NSO's build ID as 32 hex digits). A module of another build is refused.
    pub modules: &'static [(&'static str, &'static str)],
    /// The NVN functions the game fetches through the NVN loader, one name per line. The NVN layer answers these.
    pub nvn_functions: Fact<&'static str>,
    /// The shader groups of the shader split (`shader_groups` of a `game.tsv`: `<path part>=<group>` pairs).
    pub shader_groups: &'static str,
    /// The line the shader split prints for the kind of shaders (`shader_note`).
    pub shader_note: &'static str,
    /// Hand-written shader facts, merged over the generated rows (`shader_facts_manual.tsv`).
    pub shader_facts: Option<&'static str>,
    /// A path of the base game's romfs that every version has. The build refuses a romfs without it.
    pub romfs_marker: &'static str,
    /// The memory layout and the other run settings of the game, which the host reads from `game.json`.
    pub settings: Fact<crate::Settings>,
}

/// Questions of the build: shaders, preparation and rewriter rules.
pub trait BuildHooks: Sync {
    /// What a build without nxnative needs. `None`: the title still builds through nxnative.
    fn standalone(&self) -> Option<Standalone> {
        None
    }
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
}
