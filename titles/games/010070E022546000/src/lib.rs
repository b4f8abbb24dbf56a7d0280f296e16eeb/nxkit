//! The title module of Fate/hollow ataraxia REMASTERED for Nintendo Switch (`010070E022546000`).
//!
//! The game is update 1.0.2 b723 (v131072) on SDK 16.2.6, graphics OpenGL. The engine tree names its engine:
//! TYPE-MOON's Fuzz engine (`smash::fw`). The engine family is `fuzz` (TYPE-MOON's Fuzz engine (smash::fw)). The
//! modules are `main`, `subsdk3` and `sdk`. This module holds what the engine tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v131072, the
//! shader split settings, the romfs marker and the run settings of the engine's `game.tsv`. The game renders with
//! OpenGL, and nxkit has no OpenGL layer: the build refuses it with a note (the graphics family `opengl`). The data is
//! here for the day the layer exists. All nine services run on nxkit.
//!
//! The rewrite rules are data in `rules/010070E022546000.toml`: the build row of v131072 and the engine's SDK-wide
//! rules for the `sdk` module (15 rules: the node area of 62 MB and the 2 MB alignment of large regions, one rule per
//! reviewed implementation, and the 64 MB node area with the 32 MB alignment of the older VammManager).
//!
//! The form of the shared font is left to the core default (the TTF itself): on SDK 16 the engine serves the BFTTF form
//! when `main` imports `ScalableFontEngineHelper::Decode`, and nobody checked the imports of this game.

use nxtitle::{
    BuildHooks, Evidence, Fact, GraphicsApi, Memory, Runtime, RuntimeHooks, Service, Settings,
    Standalone, Title, TitleId,
};

/// The title module.
pub struct Fateha;

/// The one instance the registry lists.
pub static FATEHA: Fateha = Fateha;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/010070E022546000/game.tsv and the defaults of dm/games/game.py at 131bd013",
    read: "2026-11-22",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The display version is the one of the build row of v131072.
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
        display_versions: &[("6c16aa06a25504cee9cf0f4f7b5e615c", "1.0.2 b723")],
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

impl BuildHooks for Fateha {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 131_072,
            modules: &[
                ("main", "6c16aa06a25504cee9cf0f4f7b5e615c"),
                ("subsdk3", "220d36873cbb6d5dc39a9a37127ea80d"),
                ("sdk", "a5cb2175cad2ca2afdd4c13577dbd079"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "",
            shader_groups_only: false,
            shader_note: "Fate/hollow ataraxia REMASTERED has no NVN shaders (its GLSL shaders go through the GL layer)",
            shader_facts: None,
            romfs_marker: "/obb/pack11d.bin",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/010070E022546000.toml"))
    }
}

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A run of the game on the Mac checks each service.",
    },
);

impl RuntimeHooks for Fateha {
    fn runtime(&self, _service: Service) -> Option<Fact<Runtime>> {
        Some(NXKIT)
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

impl Title for Fateha {
    fn id(&self) -> TitleId {
        TitleId(0x0100_70E0_2254_6000)
    }

    fn name(&self) -> &'static str {
        "Fate/hollow ataraxia REMASTERED"
    }

    fn engine(&self) -> &'static str {
        "fuzz"
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
        let p = Profile::from_toml(FATEHA.profile().unwrap_or_default()).unwrap();
        assert_eq!(
            p.title_id.as_deref(),
            Some(FATEHA.id().to_string().as_str())
        );
        assert_eq!(p.declared.title.as_deref(), Some(FATEHA.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = FATEHA.standalone().unwrap();
        assert_eq!(s.version, 131_072);
        assert_eq!(s.modules.len(), 3);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.is_empty() || s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert_eq!(s.settings.value.display_versions[0].0, s.modules[0].1);
        assert_eq!(FATEHA.graphics().value, GraphicsApi::OpenGl);
    }

    #[test]
    fn the_rules_hold_the_build_row() {
        let text = FATEHA.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = FATEHA.standalone().unwrap();
        let row = &t.gbuild["010070E022546000"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 0);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| {
            t.rules
                .get("010070E022546000")
                .and_then(|r| r.get(m))
                .map_or(0, Vec::len)
        };
        assert_eq!(rules("sdk"), 15);
        assert_eq!(rules("main") + rules("subsdk3"), 0);
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
            .filter(|s| FATEHA.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }
}
