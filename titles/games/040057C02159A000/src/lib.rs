//! The title module of Nintendo Switch 2 Welcome Tour for Nintendo Switch 2 (`040057C02159A000`).
//!
//! The game is update ? (v262144) on SDK 20.5.29 (Ounce), graphics NVN2. The engine tree names its engine: NintendoWare
//! Bezel Engine 2.16.0. The engine family is `bezel` (NintendoWare Bezel Engine). The modules are `main`, `subsdk0`,
//! `subsdk1`, `subsdk3` and `sdk`. This module holds what the engine tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v262144, the
//! shader split settings, the romfs marker and the run settings of the engine's `game.tsv`. The NVN function list is
//! not a hand list: the build reads the names from the modules' own strings (an empty list asks for that). All nine
//! services run on nxkit.
//!
//! The rewrite rules are data in `rules/040057C02159A000.toml`: the build row of v262144 and the engine's SDK-wide
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
pub struct Welcometour;

/// The one instance the registry lists.
pub static WELCOMETOUR: Welcometour = Welcometour;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/040057C02159A000/game.tsv and the defaults of dm/games/game.py at 131bd013",
    read: "2026-11-22",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The build row of v262144 names no display version, so there is none.
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0x10_2800_0000,
            alias: 0x10_0000_0000,
            stack: 0x1000_0000,
            app_pool: 0x2_4000_0000,
            system_resource: 0x100_0000,
            ..Settings::DEFAULT.memory
        },

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

impl BuildHooks for Welcometour {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 262_144,
            modules: &[
                ("main", "6e3d11763a988c6b947510e0ae08ad00"),
                ("subsdk0", "4231539cc2c921bf968abf05ffd2cbab"),
                ("subsdk1", "d314fe6517f644ccf25d152ff194d3e1"),
                ("subsdk3", "23679a6598fb74093de7fcede0c6e5bd"),
                ("sdk", "d743946430d5252672cb63013b73a0ea"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "/movie/=movie /Archive/=archive /stream/=stream /nro/=nro /.nrr/=nrr",
            shader_groups_only: false,
            shader_note: "040057C02159A000 shaders",
            shader_facts: None,
            romfs_marker: "/Archive/flow~flow100.ounce.bea",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/040057C02159A000.toml"))
    }
}

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A run of the game on the Mac checks each service.",
    },
);

impl RuntimeHooks for Welcometour {
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

impl Title for Welcometour {
    fn id(&self) -> TitleId {
        TitleId(0x0400_57C0_2159_A000)
    }

    fn name(&self) -> &'static str {
        "Nintendo Switch 2 Welcome Tour"
    }

    fn engine(&self) -> &'static str {
        "bezel"
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
        let p = Profile::from_toml(WELCOMETOUR.profile().unwrap_or_default()).unwrap();
        assert_eq!(
            p.title_id.as_deref(),
            Some(WELCOMETOUR.id().to_string().as_str())
        );
        assert_eq!(p.declared.title.as_deref(), Some(WELCOMETOUR.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = WELCOMETOUR.standalone().unwrap();
        assert_eq!(s.version, 262_144);
        assert_eq!(s.modules.len(), 5);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.is_empty() || s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert!(s.settings.value.display_versions.is_empty());
        assert_eq!(WELCOMETOUR.graphics().value, GraphicsApi::Nvn);
    }

    #[test]
    fn the_rules_hold_the_build_row() {
        let text = WELCOMETOUR.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = WELCOMETOUR.standalone().unwrap();
        let row = &t.gbuild["040057C02159A000"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 0);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| {
            t.rules
                .get("040057C02159A000")
                .and_then(|r| r.get(m))
                .map_or(0, Vec::len)
        };
        assert_eq!(rules("sdk"), 15);
        assert_eq!(
            rules("main") + rules("subsdk0") + rules("subsdk1") + rules("subsdk3"),
            0
        );
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
            .filter(|s| WELCOMETOUR.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }
}
