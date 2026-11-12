//! The title module of Mario Party Superstars for Nintendo Switch (`01006FE013472000`).
//!
//! The game is update 1.1.1 (v131072) on SDK 11.4.3, graphics NVN. The engine tree names its engine: NintendoWare Bezel
//! Engine 1.55. No engine family of nxkit holds it. The modules are `main`, `subsdk0`, `subsdk1` and `sdk`. This module
//! holds what the engine tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v131072, the
//! shader split settings, the romfs marker and the run settings of the engine's `game.tsv`. The NVN function list is
//! not a hand list: the build reads the names from the modules' own strings (an empty list asks for that). All nine
//! services run on nxkit.
//!
//! The rewrite rules are data in `rules/01006FE013472000.toml`: the build row of v131072 and the engine's SDK-wide
//! rules for the `sdk` module (12 implementations of the node area of 62 MB and of the 2 MB alignment of large
//! regions).
//!
//! The game loads NRO modules at run time through `nn::ro`. nxkit takes the calls over (`nxkit-svc-system`,
//! `sdk/ro.rs`); a run of this game has not checked it.
//!
//! The form of the shared font is left to the core default (the TTF itself): on SDK 11 the engine serves the BFTTF form
//! when `main` imports `ScalableFontEngineHelper::Decode`, and nobody checked the imports of this game.

use nxtitle::{
    BuildHooks, Evidence, Fact, Memory, Runtime, RuntimeHooks, Service, Settings, Standalone, Title, TitleId,
};

/// The title module.
pub struct Mps;

/// The one instance the registry lists.
pub static MPS: Mps = Mps;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/01006FE013472000/game.tsv and the defaults of dm/games/game.py at 034e91a8",
    read: "2026-11-12",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The display
/// version is the one of the build row of v131072.
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0x1_0000_0000,
            alias: 0x1000_0000,
            stack: 0x1000_0000,
            app_pool: 0xCD50_0000,
            system_resource: 0,
            ..Settings::DEFAULT.memory
        },
        display_versions: &[("2841e26d7fb8aa1783afd4b6c116d4f9", "1.1.1")],
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

impl BuildHooks for Mps {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 131_072,
            modules: &[
                ("main", "2841e26d7fb8aa1783afd4b6c116d4f9"),
                ("subsdk0", "4dd097b435891c5e0e15a65dbe47124f"),
                ("subsdk1", "507f19d1240d09d00992248e8a0fd77e"),
                ("sdk", "2d369d9999e3e10e665106f840ffff9b"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "/Archive/=archive",
            shader_groups_only: false,
            shader_note: "Mario Party Superstars shaders",
            shader_facts: None,
            romfs_marker: "/Archive/Resident.nx.bea",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/01006FE013472000.toml"))
    }
}

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A run of the game on the Mac checks each service.",
    },
);

impl RuntimeHooks for Mps {
    fn runtime(&self, _service: Service) -> Option<Fact<Runtime>> {
        Some(NXKIT)
    }
}

impl Title for Mps {
    fn id(&self) -> TitleId {
        TitleId(0x0100_6FE0_1347_2000)
    }

    fn name(&self) -> &'static str {
        "Mario Party Superstars"
    }

    fn family(&self) -> Option<&'static str> {
        None
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
        let p = Profile::from_toml(MPS.profile().unwrap_or_default()).unwrap();
        assert_eq!(p.title_id.as_deref(), Some(MPS.id().to_string().as_str()));
        assert_eq!(p.declared.title.as_deref(), Some(MPS.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = MPS.standalone().unwrap();
        assert_eq!(s.version, 131_072);
        assert_eq!(s.modules.len(), 4);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.is_empty() || s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert_eq!(s.settings.value.display_versions[0].0, s.modules[0].1);
    }

    #[test]
    fn the_rules_hold_the_build_row() {
        let text = MPS.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = MPS.standalone().unwrap();
        let row = &t.gbuild["01006FE013472000"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 0);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| t.rules.get("01006FE013472000").and_then(|r| r.get(m)).map_or(0, Vec::len);
        assert_eq!(rules("sdk"), 12);
        assert_eq!(rules("main") + rules("subsdk0") + rules("subsdk1"), 0);
        assert!(t.frame.is_empty());
        assert_eq!(text.matches("evidence = \"engine tree ").count(), text.matches("[[rule]]").count());
    }

    #[test]
    fn nxkit_runs_every_service() {
        let on: Vec<Service> = Service::ALL
            .into_iter()
            .filter(|s| MPS.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }
}
