//! The title module of Splatoon Raiders for Nintendo Switch 2 (`0400380020056000`).
//!
//! The game is version v65536 on SDK 21.4.4 (Ounce), graphics NVN2. The engine tree names its engine: Nintendo EPD
//! (sead / agl / gsys). What every game of that engine shares is in the family `agl`. The modules are `main`, `subsdk0`
//! and `sdk`. This module holds what the engine tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v65536, the
//! shader split settings, the romfs marker and the run settings of the engine's `game.tsv`. The NVN function list is
//! not a hand list: the build reads the names from the modules' own strings (an empty list asks for that). All nine
//! services run on nxkit.
//!
//! The rewrite rules are data in `rules/0400380020056000.toml`: the build row of v65536 and the engine's SDK-wide rules
//! for the `sdk` module (12 implementations of the node area of 62 MB and of the 2 MB alignment of large regions).
//!
//! The form of the shared font is left to the core default (the TTF itself): on SDK 21 the engine serves the BFTTF form
//! when `main` imports `ScalableFontEngineHelper::Decode`, and nobody checked the imports of this game.

use nxtitle::{
    BuildHooks, Evidence, Fact, Memory, Runtime, RuntimeHooks, Service, Settings, Standalone, Title, TitleId,
};

/// The title module.
pub struct Splatraiders;

/// The one instance the registry lists.
pub static SPLATRAIDERS: Splatraiders = Splatraiders;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/0400380020056000/game.tsv and the defaults of dm/games/game.py at 034e91a8",
    read: "2026-11-12",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The display
/// version is the one of the build row of v65536.
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0x5_5000_0000,
            alias: 0x4_0000_0000,
            stack: 0x1000_0000,
            app_pool: 0x2_4000_0000,
            ..Settings::DEFAULT.memory
        },
        display_versions: &[("663c2e087a083f05307f229dfc4042d5", "v65536")],
        pad_layout: "full",
        upscaler_keys: &[
            "0253bd17deceee79",
            "085c6fddc9443e70",
            "196dfe02185a7518",
            "30d8ee33de9dee85",
            "33cc90c9d4be52c1",
            "3fe6b091bb8c40ce",
            "4acef5afefba098b",
            "546f2576bcc31ca8",
            "60f3f31a0e0132e1",
            "725bcf5c3f4bb3f3",
            "73dec8e0f4095331",
            "9cf7c6a5b13c4f4c",
            "abef20ad0d9a9b12",
            "c1b8edc09e3313b1",
            "c78798281d3519c1",
            "ddc729bcdb4c8df9",
            "e2a63ab95b10951f",
            "e97e43383ed843b6",
            "f2d6c5cfbdcf3f71",
            "f87592cca6a9dd17",
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

impl BuildHooks for Splatraiders {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 65_536,
            modules: &[
                ("main", "663c2e087a083f05307f229dfc4042d5"),
                ("subsdk0", "07e685d1c23a77a389fd205288ae904c"),
                ("sdk", "41d4de4a3b78214ad20c896960df0e5e"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "/Shader/=shader /Effect/=effect /UI/=layout",
            shader_groups_only: false,
            shader_note: "Splatoon Raiders shaders",
            shader_facts: None,
            romfs_marker: "/System/RegionLangMask.txt",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/0400380020056000.toml"))
    }
}

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A run of the game on the Mac checks each service.",
    },
);

impl RuntimeHooks for Splatraiders {
    fn runtime(&self, _service: Service) -> Option<Fact<Runtime>> {
        Some(NXKIT)
    }
}

impl Title for Splatraiders {
    fn id(&self) -> TitleId {
        TitleId(0x0400_3800_2005_6000)
    }

    fn name(&self) -> &'static str {
        "Splatoon Raiders"
    }

    /// The engine tree names the engine: Nintendo EPD (sead / agl / gsys).
    fn family(&self) -> Option<&'static str> {
        Some("agl")
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
        let p = Profile::from_toml(SPLATRAIDERS.profile().unwrap_or_default()).unwrap();
        assert_eq!(p.title_id.as_deref(), Some(SPLATRAIDERS.id().to_string().as_str()));
        assert_eq!(p.declared.title.as_deref(), Some(SPLATRAIDERS.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = SPLATRAIDERS.standalone().unwrap();
        assert_eq!(s.version, 65_536);
        assert_eq!(s.modules.len(), 3);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.is_empty() || s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert_eq!(s.settings.value.display_versions[0].0, s.modules[0].1);
    }

    #[test]
    fn the_rules_hold_the_build_row() {
        let text = SPLATRAIDERS.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = SPLATRAIDERS.standalone().unwrap();
        let row = &t.gbuild["0400380020056000"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 0);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| t.rules.get("0400380020056000").and_then(|r| r.get(m)).map_or(0, Vec::len);
        assert_eq!(rules("sdk"), 12);
        assert_eq!(rules("main") + rules("subsdk0") + rules("subsdk1"), 0);
        assert!(t.frame.is_empty());
        assert_eq!(text.matches("evidence = \"engine tree ").count(), text.matches("[[rule]]").count());
    }

    #[test]
    fn nxkit_runs_every_service() {
        let on: Vec<Service> = Service::ALL
            .into_iter()
            .filter(|s| SPLATRAIDERS.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }
}
