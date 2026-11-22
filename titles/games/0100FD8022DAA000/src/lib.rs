//! The title module of Super Mario Galaxy 2 for Nintendo Switch (`0100FD8022DAA000`).
//!
//! The game is update 1.4.0 (v393216) on SDK 18.3.5, graphics Vulkan. The engine tree names its engine: NERD's Hagi Wii
//! layer (recompiled PowerPC). The engine family is `hagi` (NERD's Hagi Wii layer). The modules are `main`, `subsdk0`
//! and `sdk`. This module holds what the engine tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v393216, the
//! shader split settings, the romfs marker and the run settings of the engine's `game.tsv`. The game has no NVN
//! shaders: its SPIR-V shaders go through the Vulkan shader step of the build. All nine services run on nxkit.
//!
//! The engine added to its Vulkan layer for this game what nxkit's does not have: triangle fans, the geometry viewport
//! stage, the stride repair of `fix_strides`, wide points and lines. The module holds the build data. The game needs
//! those parts of the layer to draw.
//!
//! The rewrite rules are data in `rules/0100FD8022DAA000.toml`: the build row of v393216 and the engine's SDK-wide
//! rules for the `sdk` module (15 rules: the node area of 62 MB and the 2 MB alignment of large regions, one rule per
//! reviewed implementation, and the 64 MB node area with the 32 MB alignment of the older VammManager).
//!
//! The form of the shared font is left to the core default (the TTF itself): on SDK 18 the engine serves the BFTTF form
//! when `main` imports `ScalableFontEngineHelper::Decode`, and nobody checked the imports of this game.

use nxtitle::{
    BuildHooks, Evidence, Fact, GraphicsApi, Memory, Runtime, RuntimeHooks, Service, Settings,
    Standalone, Title, TitleId,
};

/// The title module.
pub struct Smg2;

/// The one instance the registry lists.
pub static SMG2: Smg2 = Smg2;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/0100FD8022DAA000/game.tsv and the defaults of dm/games/game.py at 131bd013",
    read: "2026-11-22",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The display version is the one of the build row of v393216.
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0x5000_0000,
            alias: 0x2000_0000,
            stack: 0x1000_0000,
            app_pool: 0xCD50_0000,
            ..Settings::DEFAULT.memory
        },
        display_versions: &[("4c4513d976582b4a7a766401a8ea6dbf", "1.4.0")],
        pad_layout: "full",
        op_mode: "handheld",
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

impl BuildHooks for Smg2 {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 393_216,
            modules: &[
                ("main", "4c4513d976582b4a7a766401a8ea6dbf"),
                ("subsdk0", "8a72a4dcbfd375d2952288284259da94"),
                ("sdk", "6075151627016eef437c1312f2e3ed56"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "",
            shader_groups_only: false,
            shader_note: "Super Mario Galaxy 2 has no NVN shaders (its SPIR-V shaders go through the Vulkan layer)",
            shader_facts: None,
            romfs_marker: "/hagi.config",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/0100FD8022DAA000.toml"))
    }
}

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A run of the game on the Mac checks each service.",
    },
);

impl RuntimeHooks for Smg2 {
    fn runtime(&self, _service: Service) -> Option<Fact<Runtime>> {
        Some(NXKIT)
    }
}

/// The graphics API of the game, as the games table of the engine tree names it.
const GRAPHICS: Fact<GraphicsApi> = Fact::new(
    GraphicsApi::Vulkan,
    Evidence::Recorded {
        source: "engine tree dm/games/README.md at 131bd013: the Graphics column of the games table",
        read: "2026-11-22",
    },
);

impl Title for Smg2 {
    fn id(&self) -> TitleId {
        TitleId(0x0100_FD80_22DA_A000)
    }

    fn name(&self) -> &'static str {
        "Super Mario Galaxy 2"
    }

    fn engine(&self) -> &'static str {
        "hagi"
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
        let p = Profile::from_toml(SMG2.profile().unwrap_or_default()).unwrap();
        assert_eq!(p.title_id.as_deref(), Some(SMG2.id().to_string().as_str()));
        assert_eq!(p.declared.title.as_deref(), Some(SMG2.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = SMG2.standalone().unwrap();
        assert_eq!(s.version, 393_216);
        assert_eq!(s.modules.len(), 3);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.is_empty() || s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert_eq!(s.settings.value.display_versions[0].0, s.modules[0].1);
        assert_eq!(SMG2.graphics().value, GraphicsApi::Vulkan);
    }

    #[test]
    fn the_rules_hold_the_build_row() {
        let text = SMG2.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = SMG2.standalone().unwrap();
        let row = &t.gbuild["0100FD8022DAA000"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 0);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| {
            t.rules
                .get("0100FD8022DAA000")
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
            .filter(|s| SMG2.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }
}
