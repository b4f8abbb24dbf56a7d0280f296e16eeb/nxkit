//! The engine family of games on Unity.
//!
//! A fact goes here when every game on the engine shows it. A fact of one game, or of one version of the engine, stays
//! in the title module, which states the version.
//!
//! Unity 2018's `ThreadedStreamBuffer` reads with plain loads after its store-releases, and the reader and writer of
//! the render thread can both go to sleep. The rule in `rules/unity.toml` makes those loads load-acquires. It matches
//! the four functions of Unity 2018 by their fingerprints, so it cuts only a Unity 2018 build and the rewriter skips it
//! in the builds of other versions.

use nxtitle::{BuildHooks, Family, Layer, Match, RuntimeHooks, TitleFacts};

/// The family.
pub struct Engine;

/// The one instance the registry lists.
pub static ENGINE: Engine = Engine;

impl BuildHooks for Engine {
    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/unity.toml"))
    }
}

impl RuntimeHooks for Engine {}

impl Family for Engine {
    fn id(&self) -> &'static str {
        "unity"
    }

    fn name(&self) -> &'static str {
        "Unity"
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
        assert_eq!(ENGINE.id(), "unity");
        assert_eq!(ENGINE.layer(), Layer::Engine);
        assert_eq!(
            ENGINE.detect(&TitleFacts::new(nxtitle::TitleId(0))),
            Match::Unknown
        );
    }

    #[test]
    fn the_rule_of_unity_2018_names_its_evidence() {
        let text = ENGINE.rewrite_rules().unwrap_or_default();
        assert!(text.contains("scope = \"unity\""));
        assert_eq!(text.matches("[[rule]]").count(), 1);
        assert_eq!(
            text.matches("evidence = \"engine tree dm/tools/engine_patches.py")
                .count(),
            1
        );
    }
}
