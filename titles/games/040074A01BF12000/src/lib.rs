//! The title module of Donkey Kong Bananza for Nintendo Switch 2 (`040074A01BF12000`).
//!
//! The game is version v458752 on SDK 20.5.19 (Ounce), graphics NVN2. The engine tree names its engine: Nintendo EPD
//! (sead / agl / gsys). What every game of that engine shares is in the family `agl`. The modules are `main` and `sdk`.
//! This module holds what the engine tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v458752, the
//! shader split settings, the romfs marker and the run settings of the engine's `game.tsv`. The NVN function list is
//! not a hand list: the build reads the names from the modules' own strings (an empty list asks for that). All nine
//! services run on nxkit.
//!
//! The rewrite rules are data in `rules/040074A01BF12000.toml`: the build row of v458752 and the engine's SDK-wide
//! rules for the `sdk` module (12 implementations of the node area of 62 MB and of the 2 MB alignment of large
//! regions).
//!
//! The form of the shared font is left to the core default (the TTF itself): on SDK 20 the engine serves the BFTTF form
//! when `main` imports `ScalableFontEngineHelper::Decode`, and nobody checked the imports of this game.

use nxtitle::{
    BuildHooks, Evidence, Fact, Memory, Runtime, RuntimeHooks, Service, Settings, Standalone,
    Title, TitleId,
};

/// The title module.
pub struct Dkbananza;

/// The one instance the registry lists.
pub static DKBANANZA: Dkbananza = Dkbananza;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/040074A01BF12000/game.tsv and the defaults of dm/games/game.py at 034e91a8",
    read: "2026-11-12",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The display
/// version is the one of the build row of v458752.
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0x2_7000_0000,
            alias: 0x1000_0000,
            stack: 0x1000_0000,
            app_pool: 0x2_4000_0000,
            system_resource: 0,
            ..Settings::DEFAULT.memory
        },
        display_versions: &[("e25c90c1c136638885659fd55bf8a03b", "v458752")],
        pad_layout: "full",
        upscaler_keys: &[
            "06946f57dd8833c3",
            "0e351bd8ef324466",
            "131d7dcc054f92b9",
            "14a8aaf3e0a9edbd",
            "1a4c427f99c58569",
            "1c144a902af468de",
            "3b716012fcd10575",
            "446761e211ec30d8",
            "5c5876712b1ebd78",
            "5e0110401b4a3d8f",
            "75e2ecd80c1e9fb8",
            "7629d857a0a93833",
            "7a8dcb28fa38987d",
            "7ae23d53ffb1c2ff",
            "8df4d4ba8fa94292",
            "9e5597e2fcfc3c2f",
            "a9fed0568668bf97",
            "c56db3d5a0f1b5af",
            "c8910045dc5b59af",
            "d19ea940b265456f",
            "eb0af4c68c522092",
            "f13b72178b0d5e9f",
            "f585ba048398f697",
            "f8a6a2e42fe156a6",
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

impl BuildHooks for Dkbananza {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 458_752,
            modules: &[
                ("main", "e25c90c1c136638885659fd55bf8a03b"),
                ("sdk", "18265daea2105b31f25346adfbbd09b5"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "/Shader/=shader /Effect/=effect /UI/=layout",
            shader_groups_only: false,
            shader_note: "Donkey Kong Bananza shaders",
            shader_facts: None,
            romfs_marker: "/System/Resource/ResourceSizeTable.Product.rsizetable.zs",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/040074A01BF12000.toml"))
    }
}

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A run of the game on the Mac checks each service.",
    },
);

impl RuntimeHooks for Dkbananza {
    fn runtime(&self, _service: Service) -> Option<Fact<Runtime>> {
        Some(NXKIT)
    }
}

impl Title for Dkbananza {
    fn id(&self) -> TitleId {
        TitleId(0x0400_74A0_1BF1_2000)
    }

    fn name(&self) -> &'static str {
        "Donkey Kong Bananza"
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
        let p = Profile::from_toml(DKBANANZA.profile().unwrap_or_default()).unwrap();
        assert_eq!(
            p.title_id.as_deref(),
            Some(DKBANANZA.id().to_string().as_str())
        );
        assert_eq!(p.declared.title.as_deref(), Some(DKBANANZA.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = DKBANANZA.standalone().unwrap();
        assert_eq!(s.version, 458_752);
        assert_eq!(s.modules.len(), 2);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.is_empty() || s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert_eq!(s.settings.value.display_versions[0].0, s.modules[0].1);
    }

    #[test]
    fn the_rules_hold_the_build_row() {
        let text = DKBANANZA.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = DKBANANZA.standalone().unwrap();
        let row = &t.gbuild["040074A01BF12000"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 0);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| {
            t.rules
                .get("040074A01BF12000")
                .and_then(|r| r.get(m))
                .map_or(0, Vec::len)
        };
        assert_eq!(rules("sdk"), 12);
        assert_eq!(rules("main") + rules("subsdk0") + rules("subsdk1"), 0);
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
            .filter(|s| DKBANANZA.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }
}
