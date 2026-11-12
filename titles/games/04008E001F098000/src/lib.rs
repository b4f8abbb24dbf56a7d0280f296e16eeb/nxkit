//! The title module of Nintendo Switch Sports Resort for Nintendo Switch 2 (`04008E001F098000`).
//!
//! The game is update ? (v65536) on SDK 22.2.10 (Ounce), graphics NVN2. The engine tree names its engine: sead / agl /
//! gsys, Havok. What every game of that engine shares is in the family `agl`. The modules are `main`, `subsdk0` and
//! `sdk`. This module holds what the engine tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v65536, the
//! shader split settings, the romfs marker and the run settings of the engine's `game.tsv`. The NVN function list is
//! not a hand list: the build reads the names from the modules' own strings (an empty list asks for that). All nine
//! services run on nxkit.
//!
//! The rewrite rules are data in `rules/04008E001F098000.toml`: the build row of v65536 and the engine's SDK-wide rules
//! for the `sdk` module (12 implementations of the node area of 62 MB and of the 2 MB alignment of large regions).
//!
//! The form of the shared font is left to the core default (the TTF itself): on SDK 22 the engine serves the BFTTF form
//! when `main` imports `ScalableFontEngineHelper::Decode`, and nobody checked the imports of this game.

use nxtitle::{
    BuildHooks, Evidence, Fact, Memory, Runtime, RuntimeHooks, Service, Settings, Standalone,
    Title, TitleId,
};

/// The title module.
pub struct Nssr;

/// The one instance the registry lists.
pub static NSSR: Nssr = Nssr;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/04008E001F098000/game.tsv and the defaults of dm/games/game.py at 034e91a8",
    read: "2026-11-12",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The build row
/// of v65536 has `?` for the display version (the engine packed the game from a dump folder, which has no control
/// data), so the module gives none and the app shows the version number.
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0x3_1800_0000,
            alias: 0x1_1800_0000,
            stack: 0x1000_0000,
            app_pool: 0x2_4000_0000,
            ..Settings::DEFAULT.memory
        },
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

impl BuildHooks for Nssr {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 65_536,
            modules: &[
                ("main", "3941550a42845731651bcd05edbb4703"),
                ("subsdk0", "d5bc474e0cefa2140efbe3a300aff60d"),
                ("sdk", "0c5ec6021e995368f68fba79c3ed0c5b"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "/BowlingReplay/=bowlingreplay /Tex/=tex /Bake/=bake /Parameter/=parameter /Voice/=voice \
                            /Model/=model /Pack/=pack /Banc/=banc /Phive/=phive /UI/=ui /Layout/=layout /Sound/=sound \
                            /VoiceText/=voicetext /gsys/=gsys /Event/=event /Mii/=mii /Scene/=scene /System/=system \
                            /GameParameter/=gameparameter /agl/=agl /GolfSwing/=golfswing \
                            /AnimationEvent/=animationevent /CameraAnimation/=cameraanimation /Mals/=mals",
            shader_groups_only: false,
            shader_note: "Nintendo Switch Sports Resort shaders",
            shader_facts: None,
            romfs_marker: "/Tex/RBake_Scene_AnnaBoxingDesign00Scene_d1_t0.bntx.zs",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/04008E001F098000.toml"))
    }
}

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A run of the game on the Mac checks each service.",
    },
);

impl RuntimeHooks for Nssr {
    fn runtime(&self, _service: Service) -> Option<Fact<Runtime>> {
        Some(NXKIT)
    }
}

impl Title for Nssr {
    fn id(&self) -> TitleId {
        TitleId(0x0400_8E00_1F09_8000)
    }

    fn name(&self) -> &'static str {
        "Nintendo Switch Sports Resort"
    }

    /// The engine tree names the engine: sead / agl / gsys, Havok.
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
        let p = Profile::from_toml(NSSR.profile().unwrap_or_default()).unwrap();
        assert_eq!(p.title_id.as_deref(), Some(NSSR.id().to_string().as_str()));
        assert_eq!(p.declared.title.as_deref(), Some(NSSR.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = NSSR.standalone().unwrap();
        assert_eq!(s.version, 65_536);
        assert_eq!(s.modules.len(), 3);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.is_empty() || s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert!(s.settings.value.display_versions.is_empty());
    }

    #[test]
    fn the_rules_hold_the_build_row() {
        let text = NSSR.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = NSSR.standalone().unwrap();
        let row = &t.gbuild["04008E001F098000"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 0);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| {
            t.rules
                .get("04008E001F098000")
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
            .filter(|s| NSSR.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }
}
