//! The title layer: a core, engine families and title modules.
//!
//! - The core is the rest of the workspace. It knows no title and no engine.
//! - A family ([`Family`]) holds what is true for every game of one engine, for example sead, agl and gsys.
//! - A title module ([`Title`]) holds what is true for one game only.
//!
//! Families and titles answer questions through hooks ([`BuildHooks`], [`RuntimeHooks`]). Every answer is a
//! [`Fact`], a value with its [`Evidence`]. `None` means "no opinion", and the question goes one layer down:
//! title, then family, then the core default. A title names its family by id, as data. It does not depend on the
//! family crate, so either can be built and tested alone.
//!
//! The [`Catalog`] composes families and titles for one game ([`Catalog::resolve`]). The crate `nxtitles` builds
//! the catalog from the modules that are switched on by cargo features. The core uses this crate and the catalog
//! only, never a family or title crate.
//!
//! nxnative runs every [`Service`] of a game by default. A family or title moves a service to nxkit through
//! [`RuntimeHooks::runtime`], so nxkit replaces nxnative one service at a time.
//!
//! A title module does not work around a core bug. A fix that helps more than one game belongs in the core.

mod catalog;
mod evidence;
mod facts;
mod hooks;
mod runtime;

pub use catalog::{Catalog, FamilyFrom, Resolved};
pub use evidence::{Evidence, Fact};
pub use facts::{Match, TitleFacts};
pub use hooks::{
    BuildHooks, Need, PaceProfile, Pacing, PresentInterval, RuntimeHooks, Seed, ShaderRole,
    ShaderStage, ShaderView, Upscaler,
};

pub use nxb_model::TitleId;
pub use runtime::{Runtime, Service};

/// An engine family: the facts and hooks that hold for every game of one engine.
pub trait Family: BuildHooks + RuntimeHooks {
    /// The stable id that titles name (`agl`).
    fn id(&self) -> &'static str;
    /// A readable name.
    fn name(&self) -> &'static str;
    /// Whether a game belongs to this family, from facts the core read. [`Match::Unknown`] when the facts are
    /// not enough.
    fn detect(&self, facts: &TitleFacts) -> Match;
}

/// A title module: the facts and hooks for one game.
pub trait Title: BuildHooks + RuntimeHooks {
    /// The title id.
    fn id(&self) -> TitleId;
    /// The game's name.
    fn name(&self) -> &'static str;
    /// The id of the game's family, or `None` when it has none.
    fn family(&self) -> Option<&'static str>;
    /// The committed part of the game's `profile.toml` (design 2.11): `title_id`, `[declared]` and `[rules]`.
    fn profile(&self) -> Option<&'static str> {
        None
    }
}

#[cfg(test)]
mod tests;
