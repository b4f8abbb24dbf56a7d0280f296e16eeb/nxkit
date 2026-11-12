//! The title module of Diablo II: Resurrected for Nintendo Switch (`0100726014352000`).
//!
//! The game is update 1.0.34.0 (v2228224) on SDK 11.4.3, graphics NVN. The engine tree names its engine: Blizzard's D2R
//! engine, FMOD. No engine family of nxkit holds it. The modules are `main`, `subsdk0` and `sdk`. This module holds
//! what the engine tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v2228224, the
//! shader split settings, the romfs marker and the run settings of the engine's `game.tsv`. The NVN function list is
//! not a hand list: the build reads the names from the modules' own strings (an empty list asks for that). All nine
//! services run on nxkit.
//!
//! The rewrite rules are data in `rules/0100726014352000.toml`: the build row of v2228224, 2 byte cuts of the game's
//! own code from `patches.py`, 1 function-matched rule of the game from `patches.py` and the engine's SDK-wide rules
//! for the `sdk` module (12 implementations of the node area of 62 MB and of the 2 MB alignment of large regions).
//!
//! The form of the shared font is left to the core default (the TTF itself): on SDK 11 the engine serves the BFTTF form
//! when `main` imports `ScalableFontEngineHelper::Decode`, and nobody checked the imports of this game.

use nxtitle::{
    BuildHooks, Evidence, Fact, Memory, Runtime, RuntimeHooks, Service, Settings, Standalone, Title, TitleId,
};

/// The title module.
pub struct D2r;

/// The one instance the registry lists.
pub static D2R: D2r = D2r;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/0100726014352000/game.tsv and the defaults of dm/games/game.py at 034e91a8",
    read: "2026-11-12",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The display
/// version is the one of the build row of v2228224.
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0x1_0000_0000,
            alias: 0x1000_0000,
            stack: 0x1000_0000,
            app_pool: 0xCD50_0000,
            used: 0x4C0_0000,
            system_resource: 0,
            ..Settings::DEFAULT.memory
        },
        display_versions: &[("db0507f36b84200ac05f75d76ed5026f", "1.0.34.0")],
        present_display: true,
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

impl BuildHooks for D2r {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 2_228_224,
            modules: &[
                ("main", "db0507f36b84200ac05f75d76ed5026f"),
                ("subsdk0", "507f19d1240d09d00992248e8a0fd77e"),
                ("sdk", "2d369d9999e3e10e665106f840ffff9b"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "/data/=data",
            shader_groups_only: false,
            shader_note: "Diablo II: Resurrected shaders",
            shader_facts: None,
            romfs_marker: "/data/00-00000001.data",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/0100726014352000.toml"))
    }
}

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A run of the game on the Mac checks each service.",
    },
);

impl RuntimeHooks for D2r {
    fn runtime(&self, _service: Service) -> Option<Fact<Runtime>> {
        Some(NXKIT)
    }
}

impl Title for D2r {
    fn id(&self) -> TitleId {
        TitleId(0x0100_7260_1435_2000)
    }

    fn name(&self) -> &'static str {
        "Diablo II: Resurrected"
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
        let p = Profile::from_toml(D2R.profile().unwrap_or_default()).unwrap();
        assert_eq!(p.title_id.as_deref(), Some(D2R.id().to_string().as_str()));
        assert_eq!(p.declared.title.as_deref(), Some(D2R.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = D2R.standalone().unwrap();
        assert_eq!(s.version, 2_228_224);
        assert_eq!(s.modules.len(), 3);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.is_empty() || s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert_eq!(s.settings.value.display_versions[0].0, s.modules[0].1);
    }

    #[test]
    fn the_rules_hold_the_build_row() {
        let text = D2R.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = D2R.standalone().unwrap();
        let row = &t.gbuild["0100726014352000"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 2);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| t.rules.get("0100726014352000").and_then(|r| r.get(m)).map_or(0, Vec::len);
        assert_eq!(rules("sdk"), 12);
        assert_eq!(rules("main") + rules("subsdk0") + rules("subsdk1"), 1);
        assert!(t.frame.is_empty());
        assert_eq!(text.matches("evidence = \"engine tree ").count(), text.matches("[[rule]]").count());
    }

    #[test]
    fn nxkit_runs_every_service() {
        let on: Vec<Service> = Service::ALL
            .into_iter()
            .filter(|s| D2R.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }
}
