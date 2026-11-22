//! The title layer: a core, families of two layers and title modules.
//!
//! - The core is the rest of the workspace. It knows no title, no engine and no graphics API of a game.
//! - An engine family ([`Family`] of [`Layer::Engine`]) holds what is true for every game of one engine, for example
//!   sead, agl and gsys, or Unity.
//! - A graphics family ([`Family`] of [`Layer::Graphics`]) holds what is true for every game on one graphics API:
//!   NVN, Vulkan or OpenGL. Its id is the [`GraphicsApi::id`] of the API.
//! - A title module ([`Title`]) holds what is true for one game only.
//!
//! Families and titles answer questions through hooks ([`BuildHooks`], [`RuntimeHooks`]). Every answer is a
//! [`Fact`], a value with its [`Evidence`]. `None` means "no opinion", and the question goes one layer down:
//! title, then engine family, then graphics family, then the core default. A title names its engine by id, as data,
//! and its graphics API as a fact ([`Title::graphics`]). The graphics family follows from the API. A title does not
//! depend on a family crate, and no family depends on another, so each can be built and tested alone.
//!
//! The [`Catalog`] composes families and titles for one game ([`Catalog::resolve`]). The crate `nxtitles` builds
//! the catalog from the modules that are switched on by cargo features. The core uses this crate and the catalog
//! only, never a family or title crate.
//!
//! The engine runs every [`Service`] of a game by default. A family or title moves a service to nxkit through
//! [`RuntimeHooks::runtime`], so nxkit replaces the engine one service at a time.
//!
//! A title module does not work around a core bug. A fix that helps more than one game belongs in the core.

mod catalog;
mod console;
mod evidence;
mod facts;
mod hooks;
mod markers;
mod resources;
mod runtime;
mod settings;
mod temporal;
pub mod threads;

pub use catalog::{Catalog, EngineFrom, Resolved};
pub use console::{
    ConfigKey, ConsoleFiles, ConsoleSettings, ConsoleVar, RomfsEntry, RomfsRead, merge_config_keys,
    merge_console_vars,
};
pub use evidence::{Evidence, Fact};
pub use facts::{Match, TitleFacts};
pub use hooks::{
    BuildHooks, GraphicsApi, LimitKind, LimitRow, LimitRule, LimitSite, Need, PaceProfile, Pacing,
    PresentInterval, ReflectLayout, ReflectedLimit, RenderArgs, RuntimeHooks, ShaderRole,
    ShaderStage, ShaderView, Standalone, Upscaler,
};
pub use markers::{Detection, Marker, MarkerAt};

pub use nxb_model::{Digest, TitleId};
pub use resources::{Edit, ResourcePatch, ZstdDictionary};
pub use runtime::{Runtime, Service};
pub use settings::{Memory, Settings};
pub use temporal::{DrawSlot, SourceDraw, TemporalDraws, TemporalView, UboStage, VelocityCode};

/// The layer of a family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layer {
    /// An engine: a title names it with [`Title::engine`].
    Engine,
    /// A graphics API: the family of the title's [`GraphicsApi`], with its id.
    Graphics,
}

/// A family: the facts and hooks that hold for every game of one engine or of one graphics API.
pub trait Family: BuildHooks + RuntimeHooks {
    /// The stable id: the one titles name for an engine (`agl`), the [`GraphicsApi::id`] for a graphics API (`nvn`).
    fn id(&self) -> &'static str;
    /// A readable name.
    fn name(&self) -> &'static str;
    /// The layer of the family.
    fn layer(&self) -> Layer;
    /// Whether a game belongs to this family, from facts the core read. [`Match::Unknown`] when the facts are
    /// not enough.
    fn detect(&self, facts: &TitleFacts) -> Match;
    /// What the engine leaves in a game ([`Marker`]). The core finds them and gives the hits as libraries of the facts.
    /// A family with no reliable trace gives none, and its `detect` stays [`Match::Unknown`].
    fn markers(&self) -> &'static [Marker] {
        &[]
    }
    /// The id of the family that this one is a refinement of (a version of an engine against the engine). When both
    /// match, the refinement wins.
    fn refines(&self) -> Option<&'static str> {
        None
    }
}

/// A title module: the facts and hooks for one game.
pub trait Title: BuildHooks + RuntimeHooks {
    /// The title id.
    fn id(&self) -> TitleId;
    /// The game's name.
    fn name(&self) -> &'static str;
    /// The id of the game's engine family. Every title module names one, also a game on an engine no other game
    /// shares, so an engine fact always has its place. A build without that family's feature still works, with the
    /// next layer in its place.
    fn engine(&self) -> &'static str;
    /// The graphics API the game renders with. Every title module answers it: an engine can run on more than one API,
    /// so it is a fact of the game. The graphics family of the game is the one of this API.
    fn graphics(&self) -> Fact<GraphicsApi>;
    /// The committed part of the game's `profile.toml` (design 2.11): `title_id`, `[declared]` and `[rules]`.
    fn profile(&self) -> Option<&'static str> {
        None
    }
}

#[cfg(test)]
mod tests;
