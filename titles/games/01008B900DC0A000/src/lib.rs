//! The title module of FINAL FANTASY VIII Remastered for Nintendo Switch (`01008B900DC0A000`).
//!
//! The game is update 1.0.1_5 (v196608) on SDK 7.4.0, graphics OpenGL. The engine tree names its engine: BaseEngine.
//! The engine family is `baseengine` (BaseEngine). The modules are `main`, `subsdk0`, `subsdk1` and `sdk`. This module
//! holds what the engine tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v196608, the
//! shader split settings, the romfs marker and the run settings of the engine's `game.tsv`. The game renders with
//! OpenGL, and nxkit has no OpenGL layer: the build refuses it with a note (the graphics family `opengl`). The data is
//! here for the day the layer exists. All nine services run on nxkit.
//!
//! The rewrite rules are data in `rules/01008B900DC0A000.toml`: the build row of v196608 and the engine's SDK-wide
//! rules for the `sdk` module (15 rules: the node area of 62 MB and the 2 MB alignment of large regions, one rule per
//! reviewed implementation, and the 64 MB node area with the 32 MB alignment of the older VammManager).
//!
//! The shared font is the TTF itself: on SDK 10 and older the engine's rule serves it whatever `main` imports.

use nxtitle::{
    BuildHooks, Evidence, Fact, GraphicsApi, Memory, Runtime, RuntimeHooks, Service, Settings,
    Standalone, Title, TitleId,
};

/// The title module.
pub struct Ff8;

/// The one instance the registry lists.
pub static FF8: Ff8 = Ff8;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/01008B900DC0A000/game.tsv and the defaults of dm/games/game.py at 131bd013",
    read: "2026-11-22",
};

const FONT_RULE: Evidence = Evidence::Recorded {
    source: "engine tree dm/host/hle_sdk16.c (font_skip) and dm/host/dm.h (DM_SDK_OLD) at 131bd013",
    read: "2026-11-22",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The display version is the one of the build row of v196608.
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0x1_0000_0000,
            alias: 0x1000_0000,
            stack: 0x1000_0000,
            app_pool: 0xCD50_0000,
            system_resource: 0x0,
            ..Settings::DEFAULT.memory
        },
        display_versions: &[("7f59549f6e7929368d6743a882fa700a", "1.0.1_5")],
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

impl BuildHooks for Ff8 {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 196_608,
            modules: &[
                ("main", "7f59549f6e7929368d6743a882fa700a"),
                ("subsdk0", "a36dac0e0a5ed3b41482b6bb85625c73"),
                ("subsdk1", "a0492af5b413a8bbdd463a470e8991ba"),
                ("sdk", "e30d2170165e907aba457c79fd83383d"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "/weepff8/=weepff8 /.nrr/=nrr /be_loc/=beloc /font/=font",
            shader_groups_only: false,
            shader_note: "FINAL FANTASY VIII Remastered shaders",
            shader_facts: None,
            romfs_marker: "/weepff8/game_data/data/field.fs",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/01008B900DC0A000.toml"))
    }
}

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A run of the game on the Mac checks each service.",
    },
);

impl RuntimeHooks for Ff8 {
    fn runtime(&self, _service: Service) -> Option<Fact<Runtime>> {
        Some(NXKIT)
    }

    /// SDK 7: the address of the shared font is the TTF itself, whatever `main` imports.
    fn shared_font_decode(&self) -> Option<Fact<bool>> {
        Some(Fact::new(false, FONT_RULE))
    }
}

/// The graphics API of the game, as the games table of the engine tree names it.
const GRAPHICS: Fact<GraphicsApi> = Fact::new(
    GraphicsApi::OpenGl,
    Evidence::Recorded {
        source: "engine tree dm/games/README.md at 131bd013: the Graphics column of the games table",
        read: "2026-11-22",
    },
);

impl Title for Ff8 {
    fn id(&self) -> TitleId {
        TitleId(0x0100_8B90_0DC0_A000)
    }

    fn name(&self) -> &'static str {
        "FINAL FANTASY VIII Remastered"
    }

    fn engine(&self) -> &'static str {
        "baseengine"
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
        let p = Profile::from_toml(FF8.profile().unwrap_or_default()).unwrap();
        assert_eq!(p.title_id.as_deref(), Some(FF8.id().to_string().as_str()));
        assert_eq!(p.declared.title.as_deref(), Some(FF8.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = FF8.standalone().unwrap();
        assert_eq!(s.version, 196_608);
        assert_eq!(s.modules.len(), 4);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.is_empty() || s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert_eq!(s.settings.value.display_versions[0].0, s.modules[0].1);
        assert_eq!(FF8.graphics().value, GraphicsApi::OpenGl);
    }

    #[test]
    fn the_rules_hold_the_build_row() {
        let text = FF8.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = FF8.standalone().unwrap();
        let row = &t.gbuild["01008B900DC0A000"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 0);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| {
            t.rules
                .get("01008B900DC0A000")
                .and_then(|r| r.get(m))
                .map_or(0, Vec::len)
        };
        assert_eq!(rules("sdk"), 15);
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
            .filter(|s| FF8.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }
}
