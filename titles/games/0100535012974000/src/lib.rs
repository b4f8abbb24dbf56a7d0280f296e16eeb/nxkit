//! The title module of Hades for Nintendo Switch (`0100535012974000`).
//!
//! The game is update 1.0.38233 (v589824) on SDK 9.3.0, Supergiant Games' engine on The Forge renderer, graphics
//! **Vulkan**: `main` imports about 100 `vk*` functions, which `subsdk0` (the Vulkan loader) supplies on top of
//! `subsdk1` (NVIDIA's driver). The host's Vulkan layer answers them, so the modules are `main` and `sdk` only. No
//! engine family of nxkit holds the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v589824, the
//! romfs marker and the run settings of the engine's `game.tsv`. The game has no NVN shaders: its 74 SPIR-V shaders
//! (`Content/EffectsForgeSwitch/*.vert`, `*.frag`) go through the Vulkan shader step of the build. All nine services
//! run on nxkit.
//!
//! The rewrite rules are data in `rules/0100535012974000.toml`: the build row of v589824 and the engine's SDK-wide
//! rules for the `sdk` module. `main` imports Epic Online Services (`EOS_*`, from an NRO through `nn::ro`); they stay
//! offline, as on the engine.
//!
//! The shared font is left to the core default: on SDK 10 and older the engine's rule is the TTF itself.

use nxtitle::{
    BuildHooks, Evidence, Fact, GraphicsApi, Memory, Runtime, RuntimeHooks, Service, Settings,
    Standalone, Title, TitleId,
};

/// The title module.
pub struct Hades;

/// The one instance the registry lists.
pub static HADES: Hades = Hades;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/0100535012974000/game.tsv and the defaults of dm/games/game.py at 034e91a8",
    read: "2026-11-21",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The display
/// version is the one of the build row of v589824. The memory partition is Link's Awakening's (the game's README: not
/// measured for this game).
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0x1_0000_0000,
            alias: 0x1000_0000,
            stack: 0x1000_0000,
            app_pool: 0xCD50_0000,
            ..Settings::DEFAULT.memory
        },
        display_versions: &[("d41357a3da356b09fa400b90f303b564", "1.0.38233")],
        pad_layout: "full",
        ..Settings::DEFAULT
    },
    GAME_TSV,
);

/// A game on Vulkan fetches no NVN function.
const NVN_FUNCTIONS: Fact<&'static str> = Fact::new(
    "",
    Evidence::Recorded {
        source: "engine tree dm/games/0100535012974000/README.md at 034e91a8: main imports Vulkan, not nvnBootstrapLoader",
        read: "2026-11-21",
    },
);

const VULKAN: Fact<GraphicsApi> = Fact::new(
    GraphicsApi::Vulkan,
    Evidence::Recorded {
        source: "engine tree dm/games/0100535012974000/README.md at 034e91a8: main imports 100 vk* functions and \
                 not nvnBootstrapLoader; the engine played the game on its Vulkan layer",
        read: "2026-11-21",
    },
);

impl BuildHooks for Hades {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 589824,
            modules: &[
                ("main", "d41357a3da356b09fa400b90f303b564"),
                ("sdk", "230994acc1eb0f13878dd155361beff3"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "",
            shader_groups_only: false,
            shader_note: "Hades has no NVN shaders (its SPIR-V shaders go through the Vulkan layer)",
            shader_facts: None,
            romfs_marker: "/Content/EffectsForgeSwitch/Default.vert",
            settings: SETTINGS,
        })
    }

    fn graphics_api(&self) -> Option<Fact<GraphicsApi>> {
        Some(VULKAN)
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/0100535012974000.toml"))
    }
}

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A run of the game on the Mac checks each service.",
    },
);

impl RuntimeHooks for Hades {
    fn runtime(&self, _service: Service) -> Option<Fact<Runtime>> {
        Some(NXKIT)
    }
}

impl Title for Hades {
    fn id(&self) -> TitleId {
        TitleId(0x0100_5350_1297_4000)
    }

    fn name(&self) -> &'static str {
        "Hades"
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
        let p = Profile::from_toml(HADES.profile().unwrap_or_default()).unwrap();
        assert_eq!(p.title_id.as_deref(), Some(HADES.id().to_string().as_str()));
        assert_eq!(p.declared.title.as_deref(), Some(HADES.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = HADES.standalone().unwrap();
        assert_eq!(s.version, 589824);
        let names: Vec<_> = s.modules.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["main", "sdk"]);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert_eq!(s.settings.value.display_versions[0].0, s.modules[0].1);
    }

    #[test]
    fn the_game_renders_with_vulkan() {
        assert_eq!(
            HADES.graphics_api().map(|f| f.value),
            Some(GraphicsApi::Vulkan)
        );
    }

    #[test]
    fn the_rules_hold_the_build_row() {
        let text = HADES.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = HADES.standalone().unwrap();
        let row = &t.gbuild["0100535012974000"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert!(!row.iter().any(|c| c.starts_with("subsdk")));
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 0);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| {
            t.rules
                .get("0100535012974000")
                .and_then(|r| r.get(m))
                .map_or(0, Vec::len)
        };
        assert_eq!(rules("sdk"), 12);
        assert_eq!(rules("main"), 0);
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
            .filter(|s| HADES.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }
}
