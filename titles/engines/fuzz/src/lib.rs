//! The engine family of games on TYPE-MOON's Fuzz engine (smash::fw).
//!
//! A fact goes here when every game on the engine shows it. A fact of one game, or of one version of the engine, stays
//! in the title module, which states the version.
//!
//! The family holds no fact yet. It is the place for one, so that a fact found on one game of the engine is not kept in
//! that title module.

use nxtitle::{BuildHooks, Family, Layer, Match, RuntimeHooks, TitleFacts};

/// The family.
pub struct Engine;

/// The one instance the registry lists.
pub static ENGINE: Engine = Engine;

impl BuildHooks for Engine {}

impl RuntimeHooks for Engine {}

impl Family for Engine {
    fn id(&self) -> &'static str {
        "fuzz"
    }

    fn name(&self) -> &'static str {
        "TYPE-MOON's Fuzz engine (smash::fw)"
    }

    fn layer(&self) -> Layer {
        Layer::Engine
    }

    /// The core reads no fact that tells this engine: a title module names it.
    fn detect(&self, _facts: &TitleFacts) -> Match {
        Match::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_family_is_an_engine() {
        assert_eq!(ENGINE.id(), "fuzz");
        assert_eq!(ENGINE.layer(), Layer::Engine);
        assert_eq!(
            ENGINE.detect(&TitleFacts::new(nxtitle::TitleId(0))),
            Match::Unknown
        );
    }
}
