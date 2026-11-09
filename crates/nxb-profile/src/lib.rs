//! The per-game profile as data (design 2.11): one `profile.toml` per title id with three sections.
//!
//! - `[computed]`: facts read from the title, never committed.
//! - `[declared]`: few committed choices, each override with the capability it overrides and the test that proved it.
//! - `[rules]`: bindings for the rewriter and the host (engine, frame limit, byte cuts with a reason).
//!
//! One interface for every reader: [`Profile::from_toml`], [`Profile::to_toml`], [`Profile::load`], [`Profile::get`]
//! (dotted path) and [`Profile::problems`] (the rules of 2.11). Std only. The TOML reader handles the subset a profile
//! needs (see [`toml`]).

mod profile;
pub mod toml;

pub use profile::{
    CAPABILITY_KEYS, Computed, Cut, Declared, FrameLimit, PROVEN_KEYS, Present, Profile,
    ProfileError, Proof, Rules,
};

impl Profile {
    /// Read `profile.toml` from a file.
    pub fn load(path: &std::path::Path) -> Result<Profile, ProfileError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ProfileError::Shape(format!("{}: {e}", path.display())))?;
        Profile::from_toml(&text)
    }
}

#[cfg(test)]
mod tests;
