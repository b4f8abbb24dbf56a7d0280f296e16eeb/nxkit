//! The title module of Super Mario Bros. Wonder – Nintendo Switch 2 Edition + Meetup in Bellabel Park for Nintendo
//! Switch 2 (`040015100B514001`).
//!
//! The game is update 1.2.1 (v0) on SDK 21.4.1 (Ounce), graphics NVN2. The engine tree names its engine: Nintendo EPD
//! (sead / agl). The engine family is `agl` (Nintendo EPD engine (sead, agl, gsys)). The modules are `main`, `subsdk0`
//! and `sdk`. This module holds what the engine tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v0, the shader
//! split settings, the romfs marker and the run settings of the engine's `game.tsv`. The NVN function list is not a
//! hand list: the build reads the names from the modules' own strings (an empty list asks for that). All nine services
//! run on nxkit.
//!
//! The rewrite rules are data in `rules/040015100B514001.toml`: the build row of v0 and the engine's SDK-wide rules for
//! the `sdk` module (15 rules: the node area of 62 MB and the 2 MB alignment of large regions, one rule per reviewed
//! implementation, and the 64 MB node area with the 32 MB alignment of the older VammManager).
//!
//! The form of the shared font is left to the core default (the TTF itself): on SDK 21 the engine serves the BFTTF form
//! when `main` imports `ScalableFontEngineHelper::Decode`, and nobody checked the imports of this game.

use nxtitle::{
    BuildHooks, Evidence, Fact, GraphicsApi, Memory, Runtime, RuntimeHooks, Service, Settings,
    Standalone, Title, TitleId,
};

/// The title module.
pub struct Smbw2;

/// The one instance the registry lists.
pub static SMBW2: Smbw2 = Smbw2;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/040015100B514001/game.tsv and the defaults of dm/games/game.py at 131bd013",
    read: "2026-11-22",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The display version is the one of the build row of v0.
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0x2_7800_0000,
            alias: 0x1_1800_0000,
            stack: 0x1000_0000,
            app_pool: 0x2_4000_0000,
            system_resource: 0x100_0000,
            ..Settings::DEFAULT.memory
        },
        display_versions: &[("ec47e6f8846d2a7b6892e2682845c725", "1.2.1")],
        upscaler_keys: &[
            "02c8659b06ae167a",
            "03424c49e4da00e7",
            "0bc4d7b7610d27cf",
            "1069883443428f68",
            "133dbc5d0ca8f346",
            "23521e99cc9a1615",
            "29ef46d8089be8b6",
            "339a774858e80edb",
            "3807ee44a14824bb",
            "3a4099a2801690e1",
            "3a95a8a8e341b9f0",
            "4b1e1558feabd71d",
            "4cdc2c2ff618b4a1",
            "57d01f4e2bc0aacc",
            "5e8b967256ac9e7d",
            "745560d37ffb8b5a",
            "79f59630bf467ef1",
            "8f112db3a9a0f4e7",
            "91473c3bca678c8f",
            "9d9be52029530d09",
            "9ee66b08bfa0634b",
            "a39cb4097abc1895",
            "a615e396e14a0b09",
            "ae8dbb865b77d11f",
            "b1d27e2f10440ad6",
            "cac10f231c0a8bf4",
            "cdd4ff22bf14677d",
            "d14957c0a3fd7875",
            "e753e61c14a16c9b",
            "f04e71fefd214b8a",
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

impl BuildHooks for Smbw2 {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 0,
            modules: &[
                ("main", "ec47e6f8846d2a7b6892e2682845c725"),
                ("subsdk0", "3fddebac0ad5c3f9e018dcadca9e55ed"),
                ("sdk", "31945f745022d0232d2562252641b3d3"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "/Lib/=lib /Layout/=layout /Effect/=effect",
            shader_groups_only: false,
            shader_note: "Super Mario Bros. Wonder – Nintendo Switch 2 Edition + Meetup in Bellabel Park shaders",
            shader_facts: None,
            romfs_marker: "/System/RegionLangMask.txt",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/040015100B514001.toml"))
    }
}

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A run of the game on the Mac checks each service.",
    },
);

impl RuntimeHooks for Smbw2 {
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

impl Title for Smbw2 {
    fn id(&self) -> TitleId {
        TitleId(0x0400_1510_0B51_4001)
    }

    fn name(&self) -> &'static str {
        "Super Mario Bros. Wonder – Nintendo Switch 2 Edition + Meetup in Bellabel Park"
    }

    fn engine(&self) -> &'static str {
        "agl"
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
        let p = Profile::from_toml(SMBW2.profile().unwrap_or_default()).unwrap();
        assert_eq!(p.title_id.as_deref(), Some(SMBW2.id().to_string().as_str()));
        assert_eq!(p.declared.title.as_deref(), Some(SMBW2.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = SMBW2.standalone().unwrap();
        assert_eq!(s.version, 0);
        assert_eq!(s.modules.len(), 3);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.is_empty() || s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert_eq!(s.settings.value.display_versions[0].0, s.modules[0].1);
        assert_eq!(SMBW2.graphics().value, GraphicsApi::Nvn);
    }

    #[test]
    fn the_rules_hold_the_build_row() {
        let text = SMBW2.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = SMBW2.standalone().unwrap();
        let row = &t.gbuild["040015100B514001"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 0);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| {
            t.rules
                .get("040015100B514001")
                .and_then(|r| r.get(m))
                .map_or(0, Vec::len)
        };
        assert_eq!(rules("sdk"), 15);
        assert_eq!(rules("main") + rules("subsdk0"), 0);
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
            .filter(|s| SMBW2.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }
}
