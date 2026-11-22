//! The title module of Mega Man Legacy Collection for Nintendo Switch (`01002D4007AE0000`).
//!
//! The game is update 1.0.3 (v196608) on SDK 3.7.0, graphics OpenGL. The engine tree names its engine: Digital
//! Eclipse's NES emulation. The engine family is `digital-eclipse` (Digital Eclipse's NES emulation). The modules are
//! `main` and `sdk`. This module holds what the engine tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v196608, the
//! shader split settings, the romfs marker and the run settings of the engine's `game.tsv`. The game renders with
//! OpenGL, and nxkit has no OpenGL layer: the build refuses it with a note (the graphics family `opengl`). The data is
//! here for the day the layer exists. All nine services run on nxkit.
//!
//! The rewrite rules are data in `rules/01002D4007AE0000.toml`: the build row of v196608 and the engine's SDK-wide
//! rules for the `sdk` module (15 rules: the node area of 62 MB and the 2 MB alignment of large regions, one rule per
//! reviewed implementation, and the 64 MB node area with the 32 MB alignment of the older VammManager).
//!
//! The shared font is the TTF itself: on SDK 10 and older the engine's rule serves it whatever `main` imports.

use nxtitle::{
    BuildHooks, Evidence, Fact, GraphicsApi, Memory, Runtime, RuntimeHooks, Service, Settings,
    Standalone, Title, TitleId,
};

/// The title module.
pub struct Mmlc;

/// The one instance the registry lists.
pub static MMLC: Mmlc = Mmlc;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/01002D4007AE0000/game.tsv and the defaults of dm/games/game.py at 131bd013",
    read: "2026-11-22",
};

const FONT_RULE: Evidence = Evidence::Recorded {
    source: "engine tree dm/host/hle_sdk16.c (font_skip) and dm/host/dm.h (DM_SDK_OLD) at 131bd013",
    read: "2026-11-22",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The display version is the one of the build row of v196608.
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0x1_0000_0000,
            alias: 0x1000_0000,
            stack: 0x1000_0000,
            app_pool: 0xCD50_0000,
            system_resource: 0x0,
            ..Settings::DEFAULT.memory
        },
        display_versions: &[("b33795e03f1c8f9127da013b62fbdbaf", "1.0.3")],
        ..Settings::DEFAULT
    },
    GAME_TSV,
);

/// No list here: the build reads the NVN function names from the modules.
const NVN_FUNCTIONS: Fact<&'static str> = Fact::new(
    "",
    Evidence::Unverified {
        note: "the build lists the NVN function names in the read-only data of the modules: what the game can fetch \
               through the NVN loader. A run that lists the fetched names checks the list.",
    },
);

impl BuildHooks for Mmlc {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 196_608,
            modules: &[
                ("main", "b33795e03f1c8f9127da013b62fbdbaf"),
                ("sdk", "4fea41dd7345eca3a4ce829c398227a4"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "",
            shader_groups_only: false,
            shader_note: "Mega Man Legacy Collection has no NVN shaders (its GLSL shaders go through the GL layer)",
            shader_facts: None,
            romfs_marker: "/audio/music/music_credits.wav",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/01002D4007AE0000.toml"))
    }
}

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A run of the game on the Mac checks each service.",
    },
);

impl RuntimeHooks for Mmlc {
    fn runtime(&self, _service: Service) -> Option<Fact<Runtime>> {
        Some(NXKIT)
    }

    /// SDK 3: the address of the shared font is the TTF itself, whatever `main` imports.
    fn shared_font_decode(&self) -> Option<Fact<bool>> {
        Some(Fact::new(false, FONT_RULE))
    }
}

/// The graphics API of the game, as the games table of the engine tree names it.
const GRAPHICS: Fact<GraphicsApi> = Fact::new(
    GraphicsApi::OpenGl,
    Evidence::Recorded {
        source: "engine tree dm/games/README.md at 131bd013: the Graphics column of the games table",
        read: "2026-11-22",
    },
);

impl Title for Mmlc {
    fn id(&self) -> TitleId {
        TitleId(0x0100_2D40_07AE_0000)
    }

    fn name(&self) -> &'static str {
        "Mega Man Legacy Collection"
    }

    fn engine(&self) -> &'static str {
        "digital-eclipse"
    }

    fn graphics(&self) -> Fact<GraphicsApi> {
        GRAPHICS
    }

    fn profile(&self) -> Option<&'static str> {
        Some(include_str!("../profile.toml"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxb_profile::Profile;
    use nxb_rewrite::patches::PatchTable;

    #[test]
    fn profile_matches_the_module() {
        let p = Profile::from_toml(MMLC.profile().unwrap_or_default()).unwrap();
        assert_eq!(p.title_id.as_deref(), Some(MMLC.id().to_string().as_str()));
        assert_eq!(p.declared.title.as_deref(), Some(MMLC.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = MMLC.standalone().unwrap();
        assert_eq!(s.version, 196_608);
        assert_eq!(s.modules.len(), 2);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.is_empty() || s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert_eq!(s.settings.value.display_versions[0].0, s.modules[0].1);
        assert_eq!(MMLC.graphics().value, GraphicsApi::OpenGl);
    }

    #[test]
    fn the_rules_hold_the_build_row() {
        let text = MMLC.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = MMLC.standalone().unwrap();
        let row = &t.gbuild["01002D4007AE0000"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 0);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| {
            t.rules
                .get("01002D4007AE0000")
                .and_then(|r| r.get(m))
                .map_or(0, Vec::len)
        };
        assert_eq!(rules("sdk"), 15);
        assert_eq!(rules("main"), 0);
        assert!(t.frame.is_empty());
        assert_eq!(
            text.matches("evidence = \"engine tree ").count(),
            text.matches("[[rule]]").count()
        );
    }

    #[test]
    fn nxkit_runs_every_service() {
        let on: Vec<Service> = Service::ALL
            .into_iter()
            .filter(|s| MMLC.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }
}
