//! The title module of Mario Kart World for Nintendo Switch 2 (`0400C3F00006E000`).
//!
//! The game is version v0 on SDK 19.3.3 (Ounce), graphics NVN2. The engine tree names its engine: Nintendo EPD (sead /
//! agl / gsys, Pia). What every game of that engine shares is in the family `agl`. The modules are `main`, `subsdk0`
//! and `sdk`. This module holds what the engine tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v0, the shader
//! split settings, the romfs marker and the run settings of the engine's `game.tsv`. The NVN function list is not a
//! hand list: the build reads the names from the modules' own strings (an empty list asks for that). All nine services
//! run on nxkit.
//!
//! The rewrite rules are data in `rules/0400C3F00006E000.toml`: the build row of v0 and the engine's SDK-wide rules for
//! the `sdk` module (12 implementations of the node area of 62 MB and of the 2 MB alignment of large regions).
//!
//! Left out: the function-matched frame step stub of `main` (`MATCH_FRAME_STEP`): a stub is a list of steps, not a byte
//! cut, and no rule of this module describes it.
//!
//! `game.tsv` sets `fps_target 60`, which needs the frame step stub. Without the stub the module sets 0, the game's own
//! step, as the engine does for a build without its stub.
//!
//! The form of the shared font is left to the core default (the TTF itself): on SDK 19 the engine serves the BFTTF form
//! when `main` imports `ScalableFontEngineHelper::Decode`, and nobody checked the imports of this game.

use nxtitle::{
    BuildHooks, Evidence, Fact, Memory, Runtime, RuntimeHooks, Service, Settings, Standalone, Title, TitleId,
};

/// The title module.
pub struct Mkw;

/// The one instance the registry lists.
pub static MKW: Mkw = Mkw;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/0400C3F00006E000/game.tsv and the defaults of dm/games/game.py at 034e91a8",
    read: "2026-11-12",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The display
/// version is the one of the build row of v0.
///
/// `game.tsv` sets `fps_target 60`, which needs the frame step stub. Without the stub the module sets 0, the game's own
/// step, as the engine does for a build without its stub.
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0x3_2000_0000,
            alias: 0x1_0000_0000,
            stack: 0x1000_0000,
            app_pool: 0x2_4000_0000,
            system_resource: 0x400_0000,
            ..Settings::DEFAULT.memory
        },
        display_versions: &[("9e244438c3d8ef1c703a6a85ef99c4f0", "v0")],
        pad_layout: "full",
        upscaler_keys: &[
            "051422a635fafeee",
            "0e351bd8ef324466",
            "131d7dcc054f92b9",
            "1aadf4b1fe140cc6",
            "26a45d0d86eeb07f",
            "27ed4aca06572efb",
            "2ebeef7ec37fdecf",
            "2fa9e5020f13a5a5",
            "446761e211ec30d8",
            "472f4f2b47df0ffa",
            "6269703847d1b0b8",
            "639fc8644430c165",
            "672650f11b4e2a82",
            "6dbfff7d7a7fe685",
            "81d3813d8ab8756c",
            "8b82a01f1fd62cc5",
            "91719bec44edd803",
            "9e3b5c40cf35dbf3",
            "a84041e37fade1d8",
            "aacfad2f403d993a",
            "b35f14364dbad423",
            "b401e181d9dc0a50",
            "d901a683786fec6c",
            "e0c196a9a2aa75bb",
            "eddc4da7235e31b8",
            "ee2fca64102e59e7",
            "f0c619522053c2af",
            "f50d631b1f382605",
            "fb0ca0be3aff5d62",
            "fc93a64eafaee3fb",
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

impl BuildHooks for Mkw {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 0,
            modules: &[
                ("main", "9e244438c3d8ef1c703a6a85ef99c4f0"),
                ("subsdk0", "d39a7a3d44530bb61b35690e5b0383d5"),
                ("sdk", "90ebba182862b9ecdc21fcb87b999012"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "/Shader/=shader /Effect/=effect /Lib/=lib /Layout/=layout",
            shader_groups_only: false,
            shader_note: "Mario Kart World shaders",
            shader_facts: None,
            romfs_marker: "/Movie/StaffRoll_00.webm",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/0400C3F00006E000.toml"))
    }
}

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A run of the game on the Mac checks each service.",
    },
);

impl RuntimeHooks for Mkw {
    fn runtime(&self, _service: Service) -> Option<Fact<Runtime>> {
        Some(NXKIT)
    }
}

impl Title for Mkw {
    fn id(&self) -> TitleId {
        TitleId(0x0400_C3F0_0006_E000)
    }

    fn name(&self) -> &'static str {
        "Mario Kart World"
    }

    /// The engine tree names the engine: Nintendo EPD (sead / agl / gsys, Pia).
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
        let p = Profile::from_toml(MKW.profile().unwrap_or_default()).unwrap();
        assert_eq!(p.title_id.as_deref(), Some(MKW.id().to_string().as_str()));
        assert_eq!(p.declared.title.as_deref(), Some(MKW.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = MKW.standalone().unwrap();
        assert_eq!(s.version, 0);
        assert_eq!(s.modules.len(), 3);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.is_empty() || s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert_eq!(s.settings.value.display_versions[0].0, s.modules[0].1);
    }

    #[test]
    fn the_rules_hold_the_build_row() {
        let text = MKW.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = MKW.standalone().unwrap();
        let row = &t.gbuild["0400C3F00006E000"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 0);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| t.rules.get("0400C3F00006E000").and_then(|r| r.get(m)).map_or(0, Vec::len);
        assert_eq!(rules("sdk"), 12);
        assert_eq!(rules("main") + rules("subsdk0") + rules("subsdk1"), 0);
        assert!(t.frame.is_empty());
        assert_eq!(text.matches("evidence = \"engine tree ").count(), text.matches("[[rule]]").count());
    }

    #[test]
    fn nxkit_runs_every_service() {
        let on: Vec<Service> = Service::ALL
            .into_iter()
            .filter(|s| MKW.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }
}
