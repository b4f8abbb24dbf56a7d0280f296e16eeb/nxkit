//! The engine family of games on Unreal Engine 4 and 5.
//!
//! A fact goes here when every Unreal game shows it. A title names this family with `family() == Some("unreal")`.
//! The core does not detect the engine from the modules, so the family is declared only.
//!
//! The frame limit is the console variable `t.MaxFPS`. The rewriter finds each `TAutoConsoleVariable` by its registered
//! name in any build and names its offset in the fixups of `main` (`# UE_CVAR:<name>`, `nxb-rewrite` `cvar_find_all`).
//! The key of the rule is that fixups key, so the host looks it up without knowing the engine.
//! The object holds at `+0x10` the pointer to the value: a float for the game thread, then a float for the render
//! thread. Unreal steps the game by the time that really passed, so a higher limit gives more frames at the same speed.
//! A game whose speed at a rate is checked says so with `fps_max` in its title module.

use nxtitle::{
    BuildHooks, Evidence, Fact, Family, LimitKind, LimitRow, LimitRule, LimitSite, Match,
    RuntimeHooks, TitleFacts,
};

/// The family.
pub struct Unreal;

/// The one instance the registry lists.
pub static UNREAL: Unreal = Unreal;

/// The engine tree's write of the frame limit: `ue_rows` in `dm/host/hle_fpslock.c` and the object layout in
/// `dm/host/hle_uecvar.c`.
const FPSLOCK: Evidence = Evidence::Recorded {
    source: "engine tree dm/host/hle_fpslock.c (ue_rows) and dm/host/hle_uecvar.c at 034e91a8",
    read: "2026-11-22",
};

/// `t.MaxFPS`: the value pointer at `+0x10` of the object, two float copies.
const LIMITS: &[Fact<LimitRule>] = &[Fact::new(
    LimitRule::Row(LimitRow {
        what: "Unreal t.MaxFPS",
        build: None,
        site: LimitSite::Found {
            key: "UE_CVAR:t.MaxFPS",
            add: 0x10,
        },
        kind: LimitKind::FpsF32,
        copies: 2,
    }),
    FPSLOCK,
)];

impl BuildHooks for Unreal {}

impl RuntimeHooks for Unreal {
    fn limits(&self) -> &'static [Fact<LimitRule>] {
        LIMITS
    }
}

impl Family for Unreal {
    fn id(&self) -> &'static str {
        "unreal"
    }

    fn name(&self) -> &'static str {
        "Unreal Engine 4 and 5"
    }

    fn detect(&self, _facts: &TitleFacts) -> Match {
        Match::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_limit_is_found_by_name_for_every_build() {
        let [rule] = UNREAL.limits() else {
            panic!("one rule")
        };
        assert!(rule.evidence.is_checked());
        let LimitRule::Row(row) = rule.value else {
            panic!("a row")
        };
        assert_eq!(row.build, None);
        assert_eq!(
            row.site,
            LimitSite::Found {
                key: "UE_CVAR:t.MaxFPS",
                add: 0x10
            }
        );
        assert_eq!((row.kind, row.copies), (LimitKind::FpsF32, 2));
    }
}
