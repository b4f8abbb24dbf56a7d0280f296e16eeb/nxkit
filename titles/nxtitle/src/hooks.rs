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

/// How a value in the game's memory holds the frame rate, for [`LimitRow`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitKind {
    /// A float of the frame rate.
    FpsF32,
    /// An int of the present interval in vsyncs, 1 to 4.
    VsyncsI32,
    /// A float of the milliseconds of a frame, 1000 / fps, half a millisecond less at 60.
    MsF32,
    /// An int of the frame rate.
    FpsI32,
    /// An unsigned int of system ticks for a frame, `scale / fps`.
    TicksU32 {
        /// The ticks of a second.
        scale: u32,
    },
    /// A float of the seconds of a frame.
    SecsF32,
    /// A switch that an engine reads before it takes the other fields. Written 1 at any target when it is 0.
    ModeU32,
    /// A double of the frame rate. `copies` of them follow each other.
    FpsF64,
    /// An unsigned int of the present interval in vsyncs minus one, 0 or 1.
    IntervalMinusOneU32,
    /// A word that the game presets and the host replaces, whatever the target is. Written `to` when it holds `from`.
    PinU32 {
        /// The value that the game presets.
        from: u32,
        /// The value that the host writes.
        to: u32,
    },
}

/// Where a limiter value sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitSite {
    /// An offset in the main module of one build. `ptr_off` is the offset of a pointer to the object, 0 when the field is in
    /// the module itself.
    Fixed {
        /// The offset of the pointer to the object, or 0.
        ptr_off: u64,
        /// The offset of the field in the object or the module.
        field: u64,
    },
    /// An offset that the rewriter finds by a name (`key`, for example the name of a console variable). The host adds `add`
    /// to it and reads the object pointer there.
    Found {
        /// The name that the rewriter reports.
        key: &'static str,
        /// Added to the found offset.
        add: u64,
    },
}

/// One value in the game's memory that holds the frame limit. The host writes the target into it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LimitRow {
    /// What it is, for the log.
    pub what: &'static str,
    /// The build id (32 hex digits) of the main module that the offsets are for. `None` for a [`LimitSite::Found`] row,
    /// which holds for every build.
    pub build: Option<&'static str>,
    /// Where it sits.
    pub site: LimitSite,
    /// How it holds the rate.
    pub kind: LimitKind,
    /// The copies of the value that follow each other.
    pub copies: u32,
}

/// How a reflection record of the engine is laid out in the module. All offsets are in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReflectLayout {
    /// The size of a class record. The first word is the pointer to the class name.
    pub record_size: u64,
    /// The offset in a class record of the pointer to its field list.
    pub fields_at: u64,
    /// The size of one field record. Its first word is the pointer to the field name.
    pub field_stride: u64,
    /// The offset in a field record of the field's offset in the instance.
    pub field_offset_at: u64,
    /// The offset in an instance of the pointer to its type info.
    pub instance_type_at: u64,
}

/// A frame limit that the host finds by the name of a reflected class: an int field with the rate and a float field with
/// the same rate. The host writes both into the first instance that exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReflectedLimit {
    /// The class name.
    pub class: &'static str,
    /// The name of the int field.
    pub int_field: &'static str,
    /// The name of the float field.
    pub float_field: &'static str,
    /// The layout of the records.
    pub layout: ReflectLayout,
}

/// A way to make a game follow a frame limit that the host sets (no frame step patch).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitRule {
    /// A value at a known place.
    Row(LimitRow),
    /// A reflected class.
    Reflected(ReflectedLimit),
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
    /// Whether the shaders lie only in files under `shader_groups` (`shader_scope groups`). The split then reads a
    /// compressed file outside the groups as it is, without decoding it. Set it only when a split of the whole
    /// game found every shader under the groups. `false`: the split decodes and scans every file.
    pub shader_groups_only: bool,
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
    /// The keys that steer the frame step and the presents. `None` leaves it to the next layer, and the core default is the
    /// plain case: no cap, a hold of the game's interval, no snapshots.
    fn pace_profile(&self) -> Option<Fact<PaceProfile>> {
        None
    }
    /// The values that the host writes the target frame rate into, for a game that has no frame step patch. Lists of all
    /// layers add up.
    fn limits(&self) -> &'static [Fact<LimitRule>] {
        &[]
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
