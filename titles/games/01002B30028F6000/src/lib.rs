//! The title module of Celeste for Nintendo Switch (`01002B30028F6000`).
//!
//! The game is update 1.4.1.1 (v589824) on SDK 15.3.2, graphics NVN. The engine tree names its engine: MonoGame (C# →
//! C++ by BRUTE). No engine family of nxkit holds it. The modules are `main` and `sdk`. This module holds what the
//! engine tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v589824, the
//! shader split settings, the romfs marker and the run settings of the engine's `game.tsv`. The NVN function list is
//! not a hand list: the build reads the names from the modules' own strings (an empty list asks for that). All nine
//! services run on nxkit.
//!
//! The rewrite rules are data in `rules/01002B30028F6000.toml`: the build row of v589824 and the engine's SDK-wide
//! rules for the `sdk` module (12 implementations of the node area of 62 MB and of the 2 MB alignment of large
//! regions).
//!
//! The form of the shared font is left to the core default (the TTF itself): on SDK 15 the engine serves the BFTTF form
//! when `main` imports `ScalableFontEngineHelper::Decode`, and nobody checked the imports of this game.

use nxtitle::{
    BuildHooks, Evidence, Fact, Memory, Runtime, RuntimeHooks, Service, Settings, Standalone, Title, TitleId,
};

/// The title module.
pub struct Celeste;

/// The one instance the registry lists.
pub static CELESTE: Celeste = Celeste;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/01002B30028F6000/game.tsv and the defaults of dm/games/game.py at 034e91a8",
    read: "2026-11-12",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The display
/// version is the one of the build row of v589824.
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0x1_0000_0000,
            alias: 0x1000_0000,
            stack: 0x1000_0000,
            app_pool: 0xCD50_0000,
            used: 0x640_0000,
            system_resource: 0,
            ..Settings::DEFAULT.memory
        },
        display_versions: &[("9382989af9636a1e02aeb3693094e87d", "1.4.1.1")],
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

impl BuildHooks for Celeste {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 589_824,
            modules: &[
                ("main", "9382989af9636a1e02aeb3693094e87d"),
                ("sdk", "8618895246b420329b77746b960a66ab"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "/Content/=content",
            shader_groups_only: false,
            shader_note: "Celeste shaders",
            shader_facts: None,
            romfs_marker: "/Content/FMOD/Desktop/music.bank",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/01002B30028F6000.toml"))
    }
}

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A run of the game on the Mac checks each service.",
    },
);

impl RuntimeHooks for Celeste {
    fn runtime(&self, _service: Service) -> Option<Fact<Runtime>> {
        Some(NXKIT)
    }
}

impl Title for Celeste {
    fn id(&self) -> TitleId {
        TitleId(0x0100_2B30_028F_6000)
    }

    fn name(&self) -> &'static str {
        "Celeste"
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
        let p = Profile::from_toml(CELESTE.profile().unwrap_or_default()).unwrap();
        assert_eq!(p.title_id.as_deref(), Some(CELESTE.id().to_string().as_str()));
        assert_eq!(p.declared.title.as_deref(), Some(CELESTE.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = CELESTE.standalone().unwrap();
        assert_eq!(s.version, 589_824);
        assert_eq!(s.modules.len(), 2);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.is_empty() || s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert_eq!(s.settings.value.display_versions[0].0, s.modules[0].1);
    }

    #[test]
    fn the_rules_hold_the_build_row() {
        let text = CELESTE.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = CELESTE.standalone().unwrap();
        let row = &t.gbuild["01002B30028F6000"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 0);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| t.rules.get("01002B30028F6000").and_then(|r| r.get(m)).map_or(0, Vec::len);
        assert_eq!(rules("sdk"), 12);
        assert_eq!(rules("main") + rules("subsdk0") + rules("subsdk1"), 0);
        assert!(t.frame.is_empty());
        assert_eq!(text.matches("evidence = \"engine tree ").count(), text.matches("[[rule]]").count());
    }

    #[test]
    fn nxkit_runs_every_service() {
        let on: Vec<Service> = Service::ALL
            .into_iter()
            .filter(|s| CELESTE.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }
}
