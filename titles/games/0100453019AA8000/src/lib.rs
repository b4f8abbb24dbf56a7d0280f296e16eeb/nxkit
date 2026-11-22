//! The title module of Xenoblade Chronicles X: Definitive Edition for Nintendo Switch (`0100453019AA8000`).
//!
//! The game is update 1.0.1 (v65536) on SDK 19.3.2, graphics NVN. The engine tree names its engine: Monolith's engine.
//! The engine family is `monolith` (Monolith Soft's engine). The modules are `main`, `subsdk0` and `sdk`. This module
//! holds what the engine tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v65536, the
//! shader split settings, the romfs marker and the run settings of the engine's `game.tsv`. The NVN function list is
//! not a hand list: the build reads the names from the modules' own strings (an empty list asks for that). All nine
//! services run on nxkit.
//!
//! The rewrite rules are data in `rules/0100453019AA8000.toml`: the build row of v65536, 6 byte cuts of the game's own
//! code from `patches.py`, 1 function-matched rule of the game from `patches.py` and the engine's SDK-wide rules for
//! the `sdk` module (15 rules: the node area of 62 MB and the 2 MB alignment of large regions, one rule per reviewed
//! implementation, and the 64 MB node area with the 32 MB alignment of the older VammManager). Left out: `MATCH_STUBS`
//! of `patches.py`, code that the rule files have no form for.
//!
//! The form of the shared font is left to the core default (the TTF itself): on SDK 19 the engine serves the BFTTF form
//! when `main` imports `ScalableFontEngineHelper::Decode`, and nobody checked the imports of this game.

use nxtitle::{
    BuildHooks, Evidence, Fact, GraphicsApi, Memory, Runtime, RuntimeHooks, Service, Settings,
    Standalone, Title, TitleId,
};

/// The title module.
pub struct Xcx;

/// The one instance the registry lists.
pub static XCX: Xcx = Xcx;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/0100453019AA8000/game.tsv and the defaults of dm/games/game.py at 131bd013",
    read: "2026-11-22",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The display version is the one of the build row of v65536.
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0x1_4000_0000,
            alias: 0x1_1800_0000,
            stack: 0x1000_0000,
            app_pool: 0xCD50_0000,
            map_limit: true,
            ..Settings::DEFAULT.memory
        },
        display_versions: &[("3f2425864cf226844511065f77aa3e36", "1.0.1")],
        fps_target: 60,
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

impl BuildHooks for Xcx {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 65_536,
            modules: &[
                ("main", "3f2425864cf226844511065f77aa3e36"),
                ("subsdk0", "99f9fc3860de320166ad07a6442a64f4"),
                ("sdk", "cff74470477c5fc65bb2538fd4a72441"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "/movie/=movie /sound/=sound",
            shader_groups_only: false,
            shader_note: "Xenoblade Chronicles X: Definitive Edition shaders",
            shader_facts: None,
            romfs_marker: "/sts.ard",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/0100453019AA8000.toml"))
    }
}

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A run of the game on the Mac checks each service.",
    },
);

impl RuntimeHooks for Xcx {
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

impl Title for Xcx {
    fn id(&self) -> TitleId {
        TitleId(0x0100_4530_19AA_8000)
    }

    fn name(&self) -> &'static str {
        "Xenoblade Chronicles X: Definitive Edition"
    }

    fn engine(&self) -> &'static str {
        "monolith"
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
        let p = Profile::from_toml(XCX.profile().unwrap_or_default()).unwrap();
        assert_eq!(p.title_id.as_deref(), Some(XCX.id().to_string().as_str()));
        assert_eq!(p.declared.title.as_deref(), Some(XCX.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = XCX.standalone().unwrap();
        assert_eq!(s.version, 65_536);
        assert_eq!(s.modules.len(), 3);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.is_empty() || s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert_eq!(s.settings.value.display_versions[0].0, s.modules[0].1);
        assert_eq!(XCX.graphics().value, GraphicsApi::Nvn);
    }

    #[test]
    fn the_rules_hold_the_build_row() {
        let text = XCX.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = XCX.standalone().unwrap();
        let row = &t.gbuild["0100453019AA8000"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 6);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| {
            t.rules
                .get("0100453019AA8000")
                .and_then(|r| r.get(m))
                .map_or(0, Vec::len)
        };
        assert_eq!(rules("sdk"), 15);
        assert_eq!(rules("main") + rules("subsdk0"), 1);
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
            .filter(|s| XCX.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }
}
