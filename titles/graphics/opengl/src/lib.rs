//! The graphics family of games on OpenGL (ES), through the SDK's EGL and GL driver.
//!
//! nxkit has no OpenGL layer: the build refuses a game on this API with a note that names it. The title modules of such
//! games hold their build data, so they build once the layer exists.
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
        "opengl"
    }

    fn name(&self) -> &'static str {
        "OpenGL"
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
        assert_eq!(GRAPHICS.id(), "opengl");
        assert_eq!(GRAPHICS.layer(), Layer::Graphics);
        assert_eq!(
            GRAPHICS.detect(&TitleFacts::new(nxtitle::TitleId(0))),
            Match::Unknown
        );
    }
}
