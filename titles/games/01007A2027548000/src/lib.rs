//! The title module of Trails in the Sky 2nd Chapter for Nintendo Switch (`01007A2027548000`).
//!
//! The game is update 1.0.4 (v393216) on SDK 21.4.4, graphics NVN. The engine tree names its engine: Nihon Falcom's
//! engine. The engine family is `falcom` (Nihon Falcom's engine). The modules are `main`, `subsdk0` and `sdk`. This
//! module holds what the engine tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v393216, the
//! shader split settings, the romfs marker and the run settings of the engine's `game.tsv`. The NVN function list is
//! not a hand list: the build reads the names from the modules' own strings (an empty list asks for that). All nine
//! services run on nxkit.
//!
//! The rewrite rules are data in `rules/01007A2027548000.toml`: the build row of v393216 and the engine's SDK-wide
//! rules for the `sdk` module (15 rules: the node area of 62 MB and the 2 MB alignment of large regions, one rule per
//! reviewed implementation, and the 64 MB node area with the 32 MB alignment of the older VammManager).
//!
//! The form of the shared font is left to the core default (the TTF itself): on SDK 21 the engine serves the BFTTF form
//! when `main` imports `ScalableFontEngineHelper::Decode`, and nobody checked the imports of this game.

use nxtitle::{
    BuildHooks, Evidence, Fact, GraphicsApi, Memory, Runtime, RuntimeHooks, Service, Settings,
    Standalone, Title, TitleId,
};

/// The title module.
pub struct Trailssky2;

/// The one instance the registry lists.
pub static TRAILSSKY2: Trailssky2 = Trailssky2;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/01007A2027548000/game.tsv and the defaults of dm/games/game.py at 131bd013",
    read: "2026-11-22",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The display version is the one of the build row of v393216.
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0x1_4000_0000,
            alias: 0x9800_0000,
            stack: 0x1000_0000,
            app_pool: 0xCD50_0000,
            system_resource: 0x100_0000,
            ..Settings::DEFAULT.memory
        },
        display_versions: &[("a4823d39bb21f39bf8aff69aa00a5c09", "1.0.4")],
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

impl BuildHooks for Trailssky2 {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 393_216,
            modules: &[
                ("main", "a4823d39bb21f39bf8aff69aa00a5c09"),
                ("subsdk0", "fc537f0b21c880e89d6f247d22a150ca"),
                ("sdk", "148394139d9d26ca8afd5c9ba5d30601"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "/asset/=asset /voice/=voice /voice_en/=voiceen /vfx/=vfx /se/=se /scene/=scene /script_de/=scriptde /script_en/=scripten /script_es/=scriptes /script_fr/=scriptfr /table_de/=tablede /table_en/=tableen /table_es/=tablees /table_fr/=tablefr /asset_de/=assetde /asset_fr/=assetfr /asset_es/=assetes /asset_en/=asseten /bgm1/=bgm1 /bgm2/=bgm2 /bgm3/=bgm3 /layout/=layout /system/=system /movie_en/=movieen",
            shader_groups_only: false,
            shader_note: "Trails in the Sky 2nd Chapter shaders",
            shader_facts: None,
            romfs_marker: "/movie_en/mp4/sora_2nd_ed.mp4",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/01007A2027548000.toml"))
    }
}

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A run of the game on the Mac checks each service.",
    },
);

impl RuntimeHooks for Trailssky2 {
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

impl Title for Trailssky2 {
    fn id(&self) -> TitleId {
        TitleId(0x0100_7A20_2754_8000)
    }

    fn name(&self) -> &'static str {
        "Trails in the Sky 2nd Chapter"
    }

    fn engine(&self) -> &'static str {
        "falcom"
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
        let p = Profile::from_toml(TRAILSSKY2.profile().unwrap_or_default()).unwrap();
        assert_eq!(
            p.title_id.as_deref(),
            Some(TRAILSSKY2.id().to_string().as_str())
        );
        assert_eq!(p.declared.title.as_deref(), Some(TRAILSSKY2.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = TRAILSSKY2.standalone().unwrap();
        assert_eq!(s.version, 393_216);
        assert_eq!(s.modules.len(), 3);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.is_empty() || s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert_eq!(s.settings.value.display_versions[0].0, s.modules[0].1);
        assert_eq!(TRAILSSKY2.graphics().value, GraphicsApi::Nvn);
    }

    #[test]
    fn the_rules_hold_the_build_row() {
        let text = TRAILSSKY2.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = TRAILSSKY2.standalone().unwrap();
        let row = &t.gbuild["01007A2027548000"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 0);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| {
            t.rules
                .get("01007A2027548000")
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
            .filter(|s| TRAILSSKY2.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }
}
