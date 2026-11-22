//! The graphics family of games on NVN, the graphics API of the Switch SDK, and on NVN2, its form on the Switch 2.
//!
//! A fact goes here when every game on the API shows it. The host's NVN layer, the shader split of the build and the
//! pipeline archive are core code, not facts of a game: they stay in the core.
//!
//! The family holds no fact yet. It is the place for one, so that a fact found on one game of the API is not kept in
//! that title module.

use nxtitle::{BuildHooks, Family, Layer, Match, RuntimeHooks, TitleFacts};

/// The family.
pub struct Graphics;

/// The one instance the registry lists.
pub static GRAPHICS: Graphics = Graphics;

impl BuildHooks for Graphics {}

impl RuntimeHooks for Graphics {}

impl Family for Graphics {
    fn id(&self) -> &'static str {
        "nvn"
    }

    fn name(&self) -> &'static str {
        "NVN"
    }

    fn layer(&self) -> Layer {
        Layer::Graphics
    }

    /// The core reads no fact that tells this API: a title module names it.
    fn detect(&self, _facts: &TitleFacts) -> Match {
        Match::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_family_is_a_graphics_api() {
        assert_eq!(GRAPHICS.id(), "nvn");
        assert_eq!(GRAPHICS.layer(), Layer::Graphics);
        assert_eq!(
            GRAPHICS.detect(&TitleFacts::new(nxtitle::TitleId(0))),
            Match::Unknown
        );
    }
}
