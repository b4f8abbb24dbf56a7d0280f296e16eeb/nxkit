//! The engine family of games on Unreal Engine 4 and 5.
//!
//! A fact goes here when every Unreal game shows it. A title names this family with `engine() == "unreal"`.
//! The core does not detect the engine from the modules, so the family is declared only.
//!
//! The frame limit is the console variable `t.MaxFPS`. The rewriter finds each `TAutoConsoleVariable` by its registered
//! name in any build and names its offset in the fixups of `main` (`# UE_CVAR:<name>`, `nxb-rewrite` `cvar_find_all`).
//! The key of the rule is that fixups key, so the host looks it up without knowing the engine.
//! The object holds at `+0x10` the pointer to the value: a float for the game thread, then a float for the render
//! thread. Unreal steps the game by the time that really passed, so a higher limit gives more frames at the same speed.
//! A game whose speed at a rate is checked says so with `fps_max` in its title module.
//!
//! Console variables are not read from the program arguments: a Shipping build ignores `-ExecCmds`. The family writes
//! them as `+CVars=<name>=<value>` lines of the section `[<Platform> DeviceProfile]` in a copy of the project's
//! `Config/DefaultDeviceProfiles.ini`. The copy goes into a new pak that the engine mounts after the game's paks
//! (see [`BuildHooks::console_var_files`]). Keys of the engine configuration (`BuildHooks::config_keys`) go into a copy
//! of the project's `Config/DefaultEngine.ini` in the same pak. [`CVAR_PAK`] records a run of this on a title.
//!
//! The back buffer size comes from the program arguments `-ResX=` and `-ResY=`: the Switch platform code of the engine
//! reads them in `nninitStartup` into the display size (1280x720 when absent), the platform application reports that
//! size as the display, and `UGameEngine::DetermineGameWindowResolution` gives the game window the display size. The
//! renderer scales its view by `r.ScreenPercentage` of the device profile on top of that size.
//!
//! The layout of the view uniform buffer (`FViewUniformShaderParameters`) and of the velocity changes with the engine
//! version, and the family has no version. A title states it (`RuntimeHooks::temporal_view`).

mod cvarpak;
mod ini;
mod pak;

use nxtitle::{
    BuildHooks, ConsoleFiles, ConsoleSettings, Evidence, Fact, Family, Layer, LimitKind, LimitRow,
    LimitRule, LimitSite, Match, RenderArgs, RomfsRead, RuntimeHooks, TitleFacts,
};

/// The family.
pub struct Engine;

/// The one instance the registry lists.
pub static ENGINE: Engine = Engine;

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

/// `-ResX=` and `-ResY=` in `nninitStartup`: read in OCTOPATH TRAVELER (UE 4.18) and checked by a run at 1920x1080
/// and 3840x2160.
const RES_ARGS: Evidence = Evidence::Recorded {
    source: "OCTOPATH TRAVELER v327680 main: nninitStartup main+0x2c6000 (1280x720) and main+0x2c63f4, main+0x2c642c \
             (-ResX=, -ResY=), FSwitchApplication display metrics main+0x3017c4; nxkit runs of 2026-11-22",
    read: "2026-11-22",
};

/// A hand-made pak with `r.ScreenPercentage=50` in `[Switch DeviceProfile]` rendered OCTOPATH TRAVELER at half size, and
/// `r.DefaultFeature.AntiAliasing=2` turned on temporal anti-aliasing. The pak was written by the reference tool
/// `mkpak.py` (version 4, uncompressed, the same layout as the writer in `cvarpak`).
pub const CVAR_PAK: Evidence = Evidence::Recorded {
    source: "manual run of OCTOPATH TRAVELER v327680 with Kingship/Content/Paks/Kingship-Switch_1_P.pak, written by \
             the reference tools pak.py and mkpak.py",
    read: "2026-11-22",
};

impl BuildHooks for Engine {
    fn console_var_files(
        &self,
        set: ConsoleSettings<'_>,
        romfs: &dyn RomfsRead,
    ) -> Option<Result<ConsoleFiles, String>> {
        Some(cvarpak::files(set, romfs))
    }
}

impl RuntimeHooks for Engine {
    fn limits(&self) -> &'static [Fact<LimitRule>] {
        LIMITS
    }

    fn render_args(&self) -> Option<Fact<RenderArgs>> {
        Some(Fact::new(
            RenderArgs {
                size: "-ResX={w} -ResY={h}",
            },
            RES_ARGS,
        ))
    }
}

impl Family for Engine {
    fn id(&self) -> &'static str {
        "unreal"
    }

    fn name(&self) -> &'static str {
        "Unreal Engine 4 and 5"
    }

    fn layer(&self) -> Layer {
        Layer::Engine
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
        let [rule] = ENGINE.limits() else {
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

    #[test]
    fn the_pak_route_has_run_evidence() {
        assert!(CVAR_PAK.is_checked());
        assert!(ENGINE.console_vars().is_empty());
    }

    #[test]
    fn the_back_buffer_size_is_two_program_arguments() {
        let args = ENGINE.render_args().map(|f| f.value);
        assert_eq!(
            args.map(|a| a.size_args(1920, 1080)).as_deref(),
            Some("-ResX=1920 -ResY=1080")
        );
    }
}
