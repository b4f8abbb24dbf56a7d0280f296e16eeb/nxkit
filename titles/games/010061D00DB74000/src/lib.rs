//! The title module of Ori and the Blind Forest: Definitive Edition for Nintendo Switch (`010061D00DB74000`).
//!
//! The game is update 1.0.2 (v131072) on SDK 8.2.0, graphics NVN. The engine tree names its engine: Unity 2018.4.1f1
//! IL2CPP, FMOD. No engine family of nxkit holds it. The modules are `main`, `subsdk0` and `sdk`. This module holds
//! what the engine tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v131072, the
//! shader split settings, the romfs marker and the run settings of the engine's `game.tsv`. The NVN function list is
//! not a hand list: the build reads the names from the modules' own strings (an empty list asks for that). All nine
//! services run on nxkit.
//!
//! The rewrite rules are data in `rules/010061D00DB74000.toml`: the build row of v131072, the engine's SDK-wide rules
//! for the `sdk` module (12 implementations of the node area of 62 MB and of the 2 MB alignment of large regions) and
//! the engine-wide rule of Unity 2018's ThreadedStreamBuffer for `main` (load-acquires after its store-releases).

use nxtitle::{
    BuildHooks, Evidence, Fact, Memory, Runtime, RuntimeHooks, Service, Settings, Standalone, Title, TitleId,
};

/// The title module.
pub struct Oribf;

/// The one instance the registry lists.
pub static ORIBF: Oribf = Oribf;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/010061D00DB74000/game.tsv and the defaults of dm/games/game.py at 034e91a8",
    read: "2026-11-12",
};

const FONT_RULE: Evidence = Evidence::Recorded {
    source: "engine tree dm/host/hle_sdk16.c (font_skip) and dm/host/dm.h (DM_SDK_OLD) at 034e91a8",
    read: "2026-11-12",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The display
/// version is the one of the build row of v131072.
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0x1_4000_0000,
            alias: 0x1_1800_0000,
            stack: 0x1000_0000,
            app_pool: 0xCD50_0000,
            system_resource: 0x100_0000,
            ..Settings::DEFAULT.memory
        },
        display_versions: &[("9c4f2a47916b6a5478548adc62a3a87c", "1.0.2")],
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

impl BuildHooks for Oribf {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 131_072,
            modules: &[
                ("main", "9c4f2a47916b6a5478548adc62a3a87c"),
                ("subsdk0", "8df876f80ce4ceee62360b31405c452f"),
                ("sdk", "3ae34e689d7a35f13cbf42bfd672af8b"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "/Data/=data",
            shader_groups_only: false,
            shader_note: "Ori and the Blind Forest: Definitive Edition shaders",
            shader_facts: None,
            romfs_marker: "/Data/data.unity3d",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/010061D00DB74000.toml"))
    }
}

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A run of the game on the Mac checks each service.",
    },
);

impl RuntimeHooks for Oribf {
    fn runtime(&self, _service: Service) -> Option<Fact<Runtime>> {
        Some(NXKIT)
    }

    /// SDK 8: the address of the shared font is the TTF itself, whatever `main` imports.
    fn shared_font_decode(&self) -> Option<Fact<bool>> {
        Some(Fact::new(false, FONT_RULE))
    }
}

impl Title for Oribf {
    fn id(&self) -> TitleId {
        TitleId(0x0100_61D0_0DB7_4000)
    }

    fn name(&self) -> &'static str {
        "Ori and the Blind Forest: Definitive Edition"
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
        let p = Profile::from_toml(ORIBF.profile().unwrap_or_default()).unwrap();
        assert_eq!(p.title_id.as_deref(), Some(ORIBF.id().to_string().as_str()));
        assert_eq!(p.declared.title.as_deref(), Some(ORIBF.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = ORIBF.standalone().unwrap();
        assert_eq!(s.version, 131_072);
        assert_eq!(s.modules.len(), 3);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.is_empty() || s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert_eq!(s.settings.value.display_versions[0].0, s.modules[0].1);
    }

    #[test]
    fn the_rules_hold_the_build_row() {
        let text = ORIBF.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = ORIBF.standalone().unwrap();
        let row = &t.gbuild["010061D00DB74000"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 0);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| t.rules.get("010061D00DB74000").and_then(|r| r.get(m)).map_or(0, Vec::len);
        assert_eq!(rules("sdk"), 12);
        assert_eq!(rules("main") + rules("subsdk0") + rules("subsdk1"), 1);
        assert!(t.frame.is_empty());
        assert_eq!(text.matches("evidence = \"engine tree ").count(), text.matches("[[rule]]").count());
    }

    #[test]
    fn nxkit_runs_every_service() {
        let on: Vec<Service> = Service::ALL
            .into_iter()
            .filter(|s| ORIBF.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }
}
