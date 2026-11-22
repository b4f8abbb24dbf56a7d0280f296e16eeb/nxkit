//! The title module of Kirby's Dream Buffet for Nintendo Switch (`0100A8E016236000`).
//!
//! The game is update 1.0.0 (v0) on SDK 13.3.0, graphics NVN. The engine tree names its engine: HAL Laboratory's engine
//! (Basil, NintendoWare). The engine family is `hal` (HAL Laboratory's engine). The modules are `main`, `subsdk0` and
//! `sdk`. This module holds what the engine tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v0, the shader
//! split settings, the romfs marker and the run settings of the engine's `game.tsv`. The NVN function list is not a
//! hand list: the build reads the names from the modules' own strings (an empty list asks for that). All nine services
//! run on nxkit.
//!
//! The rewrite rules are data in `rules/0100A8E016236000.toml`: the build row of v0 and the engine's SDK-wide rules for
//! the `sdk` module (15 rules: the node area of 62 MB and the 2 MB alignment of large regions, one rule per reviewed
//! implementation, and the 64 MB node area with the 32 MB alignment of the older VammManager). Left out: `MATCH_STUBS`
//! of `patches.py`, code that the rule files have no form for.
//!
//! The form of the shared font is left to the core default (the TTF itself): on SDK 13 the engine serves the BFTTF form
//! when `main` imports `ScalableFontEngineHelper::Decode`, and nobody checked the imports of this game.

use nxtitle::{
    BuildHooks, Evidence, Fact, GraphicsApi, Memory, Runtime, RuntimeHooks, Service, Settings,
    Standalone, Title, TitleId,
};

/// The title module.
pub struct Kirbydb;

/// The one instance the registry lists.
pub static KIRBYDB: Kirbydb = Kirbydb;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/0100A8E016236000/game.tsv and the defaults of dm/games/game.py at 131bd013",
    read: "2026-11-22",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The display version is the one of the build row of v0.
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
        display_versions: &[("82af4e16bbc0bec8df70cd03e99bfc8c", "1.0.0")],
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

impl BuildHooks for Kirbydb {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 0,
            modules: &[
                ("main", "82af4e16bbc0bec8df70cd03e99bfc8c"),
                ("subsdk0", "7e9f31f59e001cf5d9b546ffb233f480"),
                ("sdk", "2bf3519216f5b9d3e8ce26863c2e98b6"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "/ubin/=ubin /gfx/=gfx /msg/=msg /snd/=snd /yaml/=yaml /lyt/=lyt /hidvib/=hidvib /font/=font /ptcl/=ptcl /shader/=shader /exlyml/=exlyml /param/=param /basil/=basil /fdg/=fdg /mov/=mov",
            shader_groups_only: false,
            shader_note: "Kirby's Dream Buffet shaders",
            shader_facts: None,
            romfs_marker: "/basil/ScnCommon.bin",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/0100A8E016236000.toml"))
    }
}

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A run of the game on the Mac checks each service.",
    },
);

impl RuntimeHooks for Kirbydb {
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

impl Title for Kirbydb {
    fn id(&self) -> TitleId {
        TitleId(0x0100_A8E0_1623_6000)
    }

    fn name(&self) -> &'static str {
        "Kirby's Dream Buffet"
    }

    fn engine(&self) -> &'static str {
        "hal"
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
        let p = Profile::from_toml(KIRBYDB.profile().unwrap_or_default()).unwrap();
        assert_eq!(
            p.title_id.as_deref(),
            Some(KIRBYDB.id().to_string().as_str())
        );
        assert_eq!(p.declared.title.as_deref(), Some(KIRBYDB.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = KIRBYDB.standalone().unwrap();
        assert_eq!(s.version, 0);
        assert_eq!(s.modules.len(), 3);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.is_empty() || s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert_eq!(s.settings.value.display_versions[0].0, s.modules[0].1);
        assert_eq!(KIRBYDB.graphics().value, GraphicsApi::Nvn);
    }

    #[test]
    fn the_rules_hold_the_build_row() {
        let text = KIRBYDB.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = KIRBYDB.standalone().unwrap();
        let row = &t.gbuild["0100A8E016236000"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 0);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| {
            t.rules
                .get("0100A8E016236000")
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
            .filter(|s| KIRBYDB.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }
}
