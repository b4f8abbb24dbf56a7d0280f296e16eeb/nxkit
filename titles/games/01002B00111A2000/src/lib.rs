//! The title module of Hyrule Warriors: Age of Calamity for Nintendo Switch (`01002B00111A2000`).
//!
//! The game is update 1.3.0 (v327680) on SDK 9.6.0, graphics NVN. The engine tree names its engine: Koei Tecmo's KTGL.
//! No engine family of nxkit holds it. The modules are `main`, `subsdk0` and `sdk`. This module holds what the engine
//! tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v327680, the
//! shader split settings, the romfs marker and the run settings of the engine's `game.tsv`. The NVN function list is
//! not a hand list: the build reads the names from the modules' own strings (an empty list asks for that). All nine
//! services run on nxkit.
//!
//! The rewrite rules are data in `rules/01002B00111A2000.toml`: the build row of v327680 and the engine's SDK-wide
//! rules for the `sdk` module (12 implementations of the node area of 62 MB and of the 2 MB alignment of large
//! regions).
//!
//! Left out: the frame step stubs of `main` (`FRAME_STEP` and `MATCH_FRAME_STEP`): a stub is a list of steps, not a
//! byte cut, and no rule of this module describes it.
//!
//! `game.tsv` sets `fps_target 60`, which needs the frame step stub. Without the stub the module sets 0, the game's own
//! step, as the engine does for a build without its stub.

use nxtitle::{
    BuildHooks, Evidence, Fact, Memory, Runtime, RuntimeHooks, Service, Settings, Standalone, Title, TitleId,
};

/// The title module.
pub struct Hwaoc;

/// The one instance the registry lists.
pub static HWAOC: Hwaoc = Hwaoc;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/01002B00111A2000/game.tsv and the defaults of dm/games/game.py at 034e91a8",
    read: "2026-11-12",
};

const FONT_RULE: Evidence = Evidence::Recorded {
    source: "engine tree dm/host/hle_sdk16.c (font_skip) and dm/host/dm.h (DM_SDK_OLD) at 034e91a8",
    read: "2026-11-12",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The display
/// version is the one of the build row of v327680.
///
/// `game.tsv` sets `fps_target 60`, which needs the frame step stub. Without the stub the module sets 0, the game's own
/// step, as the engine does for a build without its stub.
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0x1_0000_0000,
            alias: 0x1000_0000,
            stack: 0x1000_0000,
            app_pool: 0xCD50_0000,
            used: 0x480_0000,
            ..Settings::DEFAULT.memory
        },
        display_versions: &[("c3cf52bf2b05d7310884971b50bcccb8", "1.3.0")],
        fps_hold: false,
        tex_flush_upload: true,
        vconv_range: true,
        pad_layout: "full",
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

impl BuildHooks for Hwaoc {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 327_680,
            modules: &[
                ("main", "c3cf52bf2b05d7310884971b50bcccb8"),
                ("subsdk0", "3ab92c3444d4ece2f52417b5d8ee86eb"),
                ("sdk", "95832210b36121e88b4f9b7b66239dad"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "/asset/system.rdb=system /asset/MaterialEditor.rdb=material /asset/FieldEditor4.rdb=field \
                            /asset/CharacterEditor.rdb=character /asset/EventMaker3.rdb=event \
                            /asset/ScreenLayout.rdb=layout /asset/RRPreview.rdb=preview \
                            /asset/KIDSSystemResource.rdb=kids",
            shader_groups_only: false,
            shader_note: "Hyrule Warriors: Age of Calamity shaders",
            shader_facts: None,
            romfs_marker: "/asset/MaterialEditor.rdb.bin",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/01002B00111A2000.toml"))
    }
}

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A run of the game on the Mac checks each service.",
    },
);

impl RuntimeHooks for Hwaoc {
    fn runtime(&self, _service: Service) -> Option<Fact<Runtime>> {
        Some(NXKIT)
    }

    /// SDK 9: the address of the shared font is the TTF itself, whatever `main` imports.
    fn shared_font_decode(&self) -> Option<Fact<bool>> {
        Some(Fact::new(false, FONT_RULE))
    }
}

impl Title for Hwaoc {
    fn id(&self) -> TitleId {
        TitleId(0x0100_2B00_111A_2000)
    }

    fn name(&self) -> &'static str {
        "Hyrule Warriors: Age of Calamity"
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
        let p = Profile::from_toml(HWAOC.profile().unwrap_or_default()).unwrap();
        assert_eq!(p.title_id.as_deref(), Some(HWAOC.id().to_string().as_str()));
        assert_eq!(p.declared.title.as_deref(), Some(HWAOC.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = HWAOC.standalone().unwrap();
        assert_eq!(s.version, 327_680);
        assert_eq!(s.modules.len(), 3);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.is_empty() || s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert_eq!(s.settings.value.display_versions[0].0, s.modules[0].1);
    }

    #[test]
    fn the_rules_hold_the_build_row() {
        let text = HWAOC.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = HWAOC.standalone().unwrap();
        let row = &t.gbuild["01002B00111A2000"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 0);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| t.rules.get("01002B00111A2000").and_then(|r| r.get(m)).map_or(0, Vec::len);
        assert_eq!(rules("sdk"), 12);
        assert_eq!(rules("main") + rules("subsdk0") + rules("subsdk1"), 0);
        assert!(t.frame.is_empty());
        assert_eq!(text.matches("evidence = \"engine tree ").count(), text.matches("[[rule]]").count());
    }

    #[test]
    fn nxkit_runs_every_service() {
        let on: Vec<Service> = Service::ALL
            .into_iter()
            .filter(|s| HWAOC.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }
}
