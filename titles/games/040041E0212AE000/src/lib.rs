//! The title module of Fast Fusion for Nintendo Switch 2 (`040041E0212AE000`).
//!
//! The game is version v262144 on SDK 20.5.13 (Ounce), graphics NVN2. The engine tree names its engine: Shin'en's
//! engine. The engine family is `shinen` (Shin'en's engine). The modules are `main` and `sdk`. This module holds what
//! the engine tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v262144, the
//! shader split settings, the romfs marker and the run settings of the engine's `game.tsv`. The NVN function list is
//! not a hand list: the build reads the names from the modules' own strings (an empty list asks for that). All nine
//! services run on nxkit.
//!
//! The rewrite rules are data in `rules/040041E0212AE000.toml`: the build row of v262144 and the engine's SDK-wide
//! rules for the `sdk` module (15 rules: the node area of 62 MB and the 2 MB alignment of large regions, one rule per
//! reviewed implementation, and the 64 MB node area with the 32 MB alignment of the older VammManager).
//!
//! The form of the shared font is left to the core default (the TTF itself): on SDK 20 the engine serves the BFTTF form
//! when `main` imports `ScalableFontEngineHelper::Decode`, and nobody checked the imports of this game.

use nxtitle::{
    BuildHooks, Evidence, Fact, GraphicsApi, Memory, Runtime, RuntimeHooks, Service, Settings,
    Standalone, Title, TitleId,
};

/// The title module.
pub struct Fastfusion;

/// The one instance the registry lists.
pub static FASTFUSION: Fastfusion = Fastfusion;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/040041E0212AE000/game.tsv and the defaults of dm/games/game.py at 131bd013",
    read: "2026-11-22",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The display version is the one of the build row of v262144.
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0x2_7000_0000,
            alias: 0x1000_0000,
            stack: 0x1000_0000,
            app_pool: 0x2_4000_0000,
            system_resource: 0x0,
            ..Settings::DEFAULT.memory
        },
        display_versions: &[("e76e60ef4fe21decb40223b5de5e6396", "v262144")],
        upscaler_keys: &[
            "11f4525fb6ccfdd0",
            "3dd952aa14b7c000",
            "5d0713975a3d3b7e",
            "6a4bd9c2b3b12347",
            "7665aaa980e1f4fc",
            "771fbd62bcda47ba",
            "805f2de96bd6d78d",
            "81ddbd3888386395",
            "8b8bd2305b784d5f",
            "8e1f2bfeed153d12",
            "9badd23a1111e0ce",
            "af4f5e6f1b0fca81",
            "b096298fba4d885e",
            "c57885ca6ff0327e",
            "cd2e47d5a9a23ee1",
            "dc61a7d1dbda5a65",
            "e3c770039b86ab4b",
            "f44a7d500658de84",
            "f809de4b7aab2c96",
            "fc2f8e5de188c665",
        ],
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

impl BuildHooks for Fastfusion {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 262_144,
            modules: &[
                ("main", "e76e60ef4fe21decb40223b5de5e6396"),
                ("sdk", "bbaa095d525b16296c21f45ea0ae560b"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "/objects/=objects /sets2/=sets2 /audio/=audio /particles/=particles /sets3/=sets3 /shaders/=shaders /menu/=menu /tracks/=tracks /sets/=sets /fonts/=fonts /bulletcache/=bulletcache /lightprobes/=lightprobes /terrain/=terrain /vibration/=vibration /materials/=materials /localize/=localize /ndsr/=ndsr /prefabs/=prefabs /scripts/=scripts /shadertextures/=shadertextures",
            shader_groups_only: false,
            shader_note: "Fast Fusion shaders",
            shader_facts: None,
            romfs_marker: "/terrain/10_materials/00_snow_normal.bc7u.tex",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/040041E0212AE000.toml"))
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

impl RuntimeHooks for Fastfusion {
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

impl Title for Fastfusion {
    fn id(&self) -> TitleId {
        TitleId(0x0400_41E0_212A_E000)
    }

    fn name(&self) -> &'static str {
        "Fast Fusion"
    }

    fn engine(&self) -> &'static str {
        "shinen"
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
        let p = Profile::from_toml(FASTFUSION.profile().unwrap_or_default()).unwrap();
        assert_eq!(
            p.title_id.as_deref(),
            Some(FASTFUSION.id().to_string().as_str())
        );
        assert_eq!(p.declared.title.as_deref(), Some(FASTFUSION.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = FASTFUSION.standalone().unwrap();
        assert_eq!(s.version, 262_144);
        assert_eq!(s.modules.len(), 2);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.is_empty() || s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert_eq!(s.settings.value.display_versions[0].0, s.modules[0].1);
        assert_eq!(FASTFUSION.graphics().value, GraphicsApi::Nvn);
    }

    #[test]
    fn the_rules_hold_the_build_row() {
        let text = FASTFUSION.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = FASTFUSION.standalone().unwrap();
        let row = &t.gbuild["040041E0212AE000"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 0);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| {
            t.rules
                .get("040041E0212AE000")
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
            .filter(|s| FASTFUSION.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }
}
