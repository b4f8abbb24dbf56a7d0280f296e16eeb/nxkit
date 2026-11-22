//! The title module of Sid Meier's Civilization VII for Nintendo Switch (`0100C3601518C000`).
//!
//! The game is update 1.5.0.1 (v1638400) on SDK 19.3.11, graphics NVN. The engine tree names its engine: Firaxis,
//! Coherent Gameface / V8, Wwise. The engine family is `firaxis` (Firaxis' engine). The modules are `main` and `sdk`.
//! This module holds what the engine tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v1638400, the
//! shader split settings, the romfs marker and the run settings of the engine's `game.tsv`. The NVN function list is
//! not a hand list: the build reads the names from the modules' own strings (an empty list asks for that). All nine
//! services run on nxkit.
//!
//! The rewrite rules are data in `rules/0100C3601518C000.toml`: the build row of v1638400 and the engine's SDK-wide
//! rules for the `sdk` module (15 rules: the node area of 62 MB and the 2 MB alignment of large regions, one rule per
//! reviewed implementation, and the 64 MB node area with the 32 MB alignment of the older VammManager).
//!
//! The form of the shared font is left to the core default (the TTF itself): on SDK 19 the engine serves the BFTTF form
//! when `main` imports `ScalableFontEngineHelper::Decode`, and nobody checked the imports of this game.

use nxtitle::{
    BuildHooks, Evidence, Fact, GraphicsApi, Memory, Runtime, RuntimeHooks, Service, Settings,
    Standalone, Title, TitleId,
};

/// The title module.
pub struct Civ7;

/// The one instance the registry lists.
pub static CIV7: Civ7 = Civ7;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/0100C3601518C000/game.tsv and the defaults of dm/games/game.py at 131bd013",
    read: "2026-11-22",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The display version is the one of the build row of v1638400.
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0x3_0000_0000,
            alias: 0x2_1800_0000,
            stack: 0x1000_0000,
            app_pool: 0xCD50_0000,
            system_resource: 0x20_0000,
            ..Settings::DEFAULT.memory
        },
        display_versions: &[("5d9401301ab2bb4917bafd939e5b3dd8", "1.5.0.1")],
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

impl BuildHooks for Civ7 {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 1_638_400,
            modules: &[
                ("main", "5d9401301ab2bb4917bafd939e5b3dd8"),
                ("sdk", "df7a3960c74c3a4803f826c587153252"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "/Base/=base /DLC/=dlc /.nrr/=nrr /nro/=nro",
            shader_groups_only: false,
            shader_note: "Sid Meier's Civilization VII shaders",
            shader_facts: None,
            romfs_marker: "/Base/modules/core/movies/2160/civ7-cinematic.webm",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/0100C3601518C000.toml"))
    }

    fn pipeline_seeds(&self) -> Option<&'static str> {
        Some(include_str!("../pipelines.seeds"))
    }
}

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A run of the game on the Mac checks each service.",
    },
);

impl RuntimeHooks for Civ7 {
    fn runtime(&self, _service: Service) -> Option<Fact<Runtime>> {
        Some(NXKIT)
    }
}

/// The graphics API of the game, as the games table of the engine tree names it.
const GRAPHICS: Fact<GraphicsApi> = Fact::new(
    GraphicsApi::Nvn,
    Evidence::Recorded {
        source: "engine tree dm/games/README.md at 131bd013: the Graphics column of the games table",
        read: "2026-11-22",
    },
);

impl Title for Civ7 {
    fn id(&self) -> TitleId {
        TitleId(0x0100_C360_1518_C000)
    }

    fn name(&self) -> &'static str {
        "Sid Meier's Civilization VII"
    }

    fn engine(&self) -> &'static str {
        "firaxis"
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
        let p = Profile::from_toml(CIV7.profile().unwrap_or_default()).unwrap();
        assert_eq!(p.title_id.as_deref(), Some(CIV7.id().to_string().as_str()));
        assert_eq!(p.declared.title.as_deref(), Some(CIV7.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = CIV7.standalone().unwrap();
        assert_eq!(s.version, 1_638_400);
        assert_eq!(s.modules.len(), 2);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.is_empty() || s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert_eq!(s.settings.value.display_versions[0].0, s.modules[0].1);
        assert_eq!(CIV7.graphics().value, GraphicsApi::Nvn);
    }

    #[test]
    fn the_rules_hold_the_build_row() {
        let text = CIV7.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = CIV7.standalone().unwrap();
        let row = &t.gbuild["0100C3601518C000"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 0);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| {
            t.rules
                .get("0100C3601518C000")
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
            .filter(|s| CIV7.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }
}
