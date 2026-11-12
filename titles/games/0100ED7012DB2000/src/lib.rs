//! The title module of ROMANCE OF THE THREE KINGDOMS XIV: Diplomacy and Strategy Expansion Pack for Nintendo Switch
//! (`0100ED7012DB2000`).
//!
//! The game is update 1.0.10 (v458752) on SDK 10.4.1, graphics NVN. The engine tree names its engine: Koei Tecmo's
//! KTGL. No engine family of nxkit holds it. The modules are `main`, `subsdk0` and `sdk`. This module holds what the
//! engine tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v458752, the
//! shader split settings, the romfs marker and the run settings of the engine's `game.tsv`. The NVN function list is
//! not a hand list: the build reads the names from the modules' own strings (an empty list asks for that). All nine
//! services run on nxkit.
//!
//! The rewrite rules are data in `rules/0100ED7012DB2000.toml`: the build row of v458752 and the engine's SDK-wide
//! rules for the `sdk` module (12 implementations of the node area of 62 MB and of the 2 MB alignment of large
//! regions).

use nxtitle::{
    BuildHooks, Evidence, Fact, Memory, Runtime, RuntimeHooks, Service, Settings, Standalone, Title, TitleId,
};

/// The title module.
pub struct Rotk14;

/// The one instance the registry lists.
pub static ROTK14: Rotk14 = Rotk14;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/0100ED7012DB2000/game.tsv and the defaults of dm/games/game.py at 034e91a8",
    read: "2026-11-12",
};

const FONT_RULE: Evidence = Evidence::Recorded {
    source: "engine tree dm/host/hle_sdk16.c (font_skip) and dm/host/dm.h (DM_SDK_OLD) at 034e91a8",
    read: "2026-11-12",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The display
/// version is the one of the build row of v458752.
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0x1_0000_0000,
            alias: 0x1000_0000,
            stack: 0x1000_0000,
            app_pool: 0xCD50_0000,
            used: 0x460_0000,
            system_resource: 0,
            ..Settings::DEFAULT.memory
        },
        display_versions: &[("1a971cc40b6d5b3f7cc3317856d822fe", "1.0.10")],
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

impl BuildHooks for Rotk14 {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 458_752,
            modules: &[
                ("main", "1a971cc40b6d5b3f7cc3317856d822fe"),
                ("subsdk0", "84e50460493a12f26100e5150fbff271"),
                ("sdk", "d9add76fc264863d85fbbd1d70076c76"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "/0002/=0002 /DLC_PATCH/=dlcpatch /0003/=0003 /0010_EN/=0010en /0003_SC/=0003sc \
                            /0004/=0004 /0000/=0000 /0003_EN/=0003en /0001_EN/=0001en",
            shader_groups_only: false,
            shader_note: "ROMANCE OF THE THREE KINGDOMS XIV: Diplomacy and Strategy Expansion Pack shaders",
            shader_facts: None,
            romfs_marker: "/0002/ScreenLayout.rdb.bin",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/0100ED7012DB2000.toml"))
    }
}

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A run of the game on the Mac checks each service.",
    },
);

impl RuntimeHooks for Rotk14 {
    fn runtime(&self, _service: Service) -> Option<Fact<Runtime>> {
        Some(NXKIT)
    }

    /// SDK 10: the address of the shared font is the TTF itself, whatever `main` imports.
    fn shared_font_decode(&self) -> Option<Fact<bool>> {
        Some(Fact::new(false, FONT_RULE))
    }
}

impl Title for Rotk14 {
    fn id(&self) -> TitleId {
        TitleId(0x0100_ED70_12DB_2000)
    }

    fn name(&self) -> &'static str {
        "ROMANCE OF THE THREE KINGDOMS XIV: Diplomacy and Strategy Expansion Pack"
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
        let p = Profile::from_toml(ROTK14.profile().unwrap_or_default()).unwrap();
        assert_eq!(p.title_id.as_deref(), Some(ROTK14.id().to_string().as_str()));
        assert_eq!(p.declared.title.as_deref(), Some(ROTK14.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = ROTK14.standalone().unwrap();
        assert_eq!(s.version, 458_752);
        assert_eq!(s.modules.len(), 3);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.is_empty() || s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert_eq!(s.settings.value.display_versions[0].0, s.modules[0].1);
    }

    #[test]
    fn the_rules_hold_the_build_row() {
        let text = ROTK14.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = ROTK14.standalone().unwrap();
        let row = &t.gbuild["0100ED7012DB2000"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 0);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| t.rules.get("0100ED7012DB2000").and_then(|r| r.get(m)).map_or(0, Vec::len);
        assert_eq!(rules("sdk"), 12);
        assert_eq!(rules("main") + rules("subsdk0") + rules("subsdk1"), 0);
        assert!(t.frame.is_empty());
        assert_eq!(text.matches("evidence = \"engine tree ").count(), text.matches("[[rule]]").count());
    }

    #[test]
    fn nxkit_runs_every_service() {
        let on: Vec<Service> = Service::ALL
            .into_iter()
            .filter(|s| ROTK14.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }
}
