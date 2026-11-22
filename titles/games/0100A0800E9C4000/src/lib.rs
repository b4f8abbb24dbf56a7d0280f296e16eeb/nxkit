//! The title module of The Witcher 3: Wild Hunt — Complete Edition for Nintendo Switch (`0100A0800E9C4000`).
//!
//! The game is update 4.04b (v458752) on SDK 15.3.0, graphics NVN. The engine tree names its engine: CD PROJEKT RED's
//! REDengine 3 (Saber Interactive port). The engine family is `redengine` (CD PROJEKT RED's REDengine 3). The modules
//! are `main` and `sdk`. This module holds what the engine tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v458752, the
//! shader split settings, the romfs marker and the run settings of the engine's `game.tsv`. The NVN function list is
//! not a hand list: the build reads the names from the modules' own strings (an empty list asks for that). All nine
//! services run on nxkit.
//!
//! The rewrite rules are data in `rules/0100A0800E9C4000.toml`: the build row of v458752 and the engine's SDK-wide
//! rules for the `sdk` module (15 rules: the node area of 62 MB and the 2 MB alignment of large regions, one rule per
//! reviewed implementation, and the 64 MB node area with the 32 MB alignment of the older VammManager).
//!
//! The form of the shared font is left to the core default (the TTF itself): on SDK 15 the engine serves the BFTTF form
//! when `main` imports `ScalableFontEngineHelper::Decode`, and nobody checked the imports of this game.
//!
//! The hand-written shader facts are the engine's `shader_facts_manual.tsv`, in `shader_facts.tsv`.

use nxtitle::{
    BuildHooks, Evidence, Fact, GraphicsApi, Memory, Runtime, RuntimeHooks, Service, Settings,
    Standalone, Title, TitleId,
};

/// The title module.
pub struct Witcher3;

/// The one instance the registry lists.
pub static WITCHER3: Witcher3 = Witcher3;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/0100A0800E9C4000/game.tsv and the defaults of dm/games/game.py at 131bd013",
    read: "2026-11-22",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The display version is the one of the build row of v458752.
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0xC_A800_0000,
            alias: 0xC_8000_0000,
            stack: 0x1000_0000,
            app_pool: 0xCD50_0000,
            system_resource: 0x40_0000,
            ..Settings::DEFAULT.memory
        },
        display_versions: &[("d27fd8a515077f340c66cec23b13c0fa", "4.04b")],
        present_display: true,
        fps_target: 60,
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

impl BuildHooks for Witcher3 {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 458_752,
            modules: &[
                ("main", "d27fd8a515077f340c66cec23b13c0fa"),
                ("sdk", "d5a1029a57d8cd4fdf4eacad87b0cfd0"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "/bin/=bin /dlc/=dlc /content/=content",
            shader_groups_only: false,
            shader_note: "The Witcher 3: Wild Hunt — Complete Edition shaders (REDengine 3 shader caches in content and the DLCs, main, loose .bin files)",
            shader_facts: Some(include_str!("../shader_facts.tsv")),
            romfs_marker: "/content/patch0/texture.cache",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/0100A0800E9C4000.toml"))
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

impl RuntimeHooks for Witcher3 {
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

impl Title for Witcher3 {
    fn id(&self) -> TitleId {
        TitleId(0x0100_A080_0E9C_4000)
    }

    fn name(&self) -> &'static str {
        "The Witcher 3: Wild Hunt — Complete Edition"
    }

    fn engine(&self) -> &'static str {
        "redengine"
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
        let p = Profile::from_toml(WITCHER3.profile().unwrap_or_default()).unwrap();
        assert_eq!(
            p.title_id.as_deref(),
            Some(WITCHER3.id().to_string().as_str())
        );
        assert_eq!(p.declared.title.as_deref(), Some(WITCHER3.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = WITCHER3.standalone().unwrap();
        assert_eq!(s.version, 458_752);
        assert_eq!(s.modules.len(), 2);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.is_empty() || s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert_eq!(s.settings.value.display_versions[0].0, s.modules[0].1);
        assert_eq!(WITCHER3.graphics().value, GraphicsApi::Nvn);
    }

    #[test]
    fn the_rules_hold_the_build_row() {
        let text = WITCHER3.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = WITCHER3.standalone().unwrap();
        let row = &t.gbuild["0100A0800E9C4000"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 0);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| {
            t.rules
                .get("0100A0800E9C4000")
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
            .filter(|s| WITCHER3.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }
}
