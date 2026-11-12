//! The title module of Pokémon Brilliant Diamond for Nintendo Switch (`0100000011D90000`).
//!
//! The game is update 1.3.0 (v393216) on SDK 11.4.3, graphics NVN. The engine tree names its engine: Unity 2019.4.27f1
//! IL2CPP. No engine family of nxkit holds it. The modules are `main`, `subsdk0` and `sdk`. This module holds what the
//! engine tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v393216, the
//! shader split settings, the romfs marker and the run settings of the engine's `game.tsv`. The NVN function list is
//! not a hand list: the build reads the names from the modules' own strings (an empty list asks for that). All nine
//! services run on nxkit.
//!
//! The rewrite rules are data in `rules/0100000011D90000.toml`: the build row of v393216 and the engine's SDK-wide
//! rules for the `sdk` module (12 implementations of the node area of 62 MB and of the 2 MB alignment of large
//! regions).
//!
//! The form of the shared font is left to the core default (the TTF itself): on SDK 11 the engine serves the BFTTF form
//! when `main` imports `ScalableFontEngineHelper::Decode`, and nobody checked the imports of this game.

use nxtitle::{
    BuildHooks, Evidence, Fact, Memory, Runtime, RuntimeHooks, Service, Settings, Standalone, Title, TitleId,
};

/// The title module.
pub struct Pkbd;

/// The one instance the registry lists.
pub static PKBD: Pkbd = Pkbd;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/0100000011D90000/game.tsv and the defaults of dm/games/game.py at 034e91a8",
    read: "2026-11-12",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The display
/// version is the one of the build row of v393216.
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0x1_4000_0000,
            alias: 0x1_1800_0000,
            stack: 0x1000_0000,
            app_pool: 0x8000_0000,
            system_resource: 0x100_0000,
            ..Settings::DEFAULT.memory
        },
        display_versions: &[("94ceae325c205c4b9d6f7235552f28fd", "1.3.0")],
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

impl BuildHooks for Pkbd {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 393_216,
            modules: &[
                ("main", "94ceae325c205c4b9d6f7235552f28fd"),
                ("subsdk0", "507f19d1240d09d00992248e8a0fd77e"),
                ("sdk", "2d369d9999e3e10e665106f840ffff9b"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "/Data/=data",
            shader_groups_only: false,
            shader_note: "Pokémon Brilliant Diamond shaders",
            shader_facts: None,
            romfs_marker: "/Data/StreamingAssets/Movie/dia/Ending/movie_dia_ed_female_00.webm",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/0100000011D90000.toml"))
    }
}

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A run of the game on the Mac checks each service.",
    },
);

impl RuntimeHooks for Pkbd {
    fn runtime(&self, _service: Service) -> Option<Fact<Runtime>> {
        Some(NXKIT)
    }
}

impl Title for Pkbd {
    fn id(&self) -> TitleId {
        TitleId(0x0100_0000_11D9_0000)
    }

    fn name(&self) -> &'static str {
        "Pokémon Brilliant Diamond"
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
        let p = Profile::from_toml(PKBD.profile().unwrap_or_default()).unwrap();
        assert_eq!(p.title_id.as_deref(), Some(PKBD.id().to_string().as_str()));
        assert_eq!(p.declared.title.as_deref(), Some(PKBD.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = PKBD.standalone().unwrap();
        assert_eq!(s.version, 393_216);
        assert_eq!(s.modules.len(), 3);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.is_empty() || s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert_eq!(s.settings.value.display_versions[0].0, s.modules[0].1);
    }

    #[test]
    fn the_rules_hold_the_build_row() {
        let text = PKBD.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = PKBD.standalone().unwrap();
        let row = &t.gbuild["0100000011D90000"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 0);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| t.rules.get("0100000011D90000").and_then(|r| r.get(m)).map_or(0, Vec::len);
        assert_eq!(rules("sdk"), 12);
        assert_eq!(rules("main") + rules("subsdk0") + rules("subsdk1"), 0);
        assert!(t.frame.is_empty());
        assert_eq!(text.matches("evidence = \"engine tree ").count(), text.matches("[[rule]]").count());
    }

    #[test]
    fn nxkit_runs_every_service() {
        let on: Vec<Service> = Service::ALL
            .into_iter()
            .filter(|s| PKBD.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }
}
