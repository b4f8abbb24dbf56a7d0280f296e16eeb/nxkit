//! The title module of Bastion for Nintendo Switch (`010038600B27E000`).
//!
//! The game is update 1.0.0 (v0) on SDK 5.4.0, graphics NVN. The engine tree names its engine: Supergiant Games' engine
//! on MonoGame 3.0. No engine family of nxkit holds it. The modules are `main` and `sdk`. This module holds what the
//! engine tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v0, the shader
//! split settings, the romfs marker and the run settings of the engine's `game.tsv`. The NVN function list is not a
//! hand list: the build reads the names from the modules' own strings (an empty list asks for that). All nine services
//! run on nxkit.
//!
//! The rewrite rules are data in `rules/010038600B27E000.toml`: the build row of v0 and the engine's SDK-wide rules for
//! the `sdk` module (12 implementations of the node area of 62 MB and of the 2 MB alignment of large regions).

use nxtitle::{
    BuildHooks, Evidence, Fact, Memory, Runtime, RuntimeHooks, Service, Settings, Standalone, Title, TitleId,
};

/// The title module.
pub struct Bastion;

/// The one instance the registry lists.
pub static BASTION: Bastion = Bastion;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/010038600B27E000/game.tsv and the defaults of dm/games/game.py at 034e91a8",
    read: "2026-11-12",
};

const FONT_RULE: Evidence = Evidence::Recorded {
    source: "engine tree dm/host/hle_sdk16.c (font_skip) and dm/host/dm.h (DM_SDK_OLD) at 034e91a8",
    read: "2026-11-12",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The display
/// version is the one of the build row of v0.
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0x1_0000_0000,
            alias: 0x1000_0000,
            stack: 0x1000_0000,
            app_pool: 0xCD50_0000,
            used: 0x4C0_0000,
            system_resource: 0,
            ..Settings::DEFAULT.memory
        },
        display_versions: &[("26a2d0ccaee5f39e92aad6bab4ba7c47", "1.0.0")],
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

impl BuildHooks for Bastion {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 0,
            modules: &[
                ("main", "26a2d0ccaee5f39e92aad6bab4ba7c47"),
                ("sdk", "26ffb122262ca25e4c016c0af2f92a29"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "/Content/=content /Resources/=resources",
            shader_groups_only: false,
            shader_note: "Bastion shaders",
            shader_facts: None,
            romfs_marker: "/Content/Audio/StreamingWaveBank.xwb",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/010038600B27E000.toml"))
    }
}

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A run of the game on the Mac checks each service.",
    },
);

impl RuntimeHooks for Bastion {
    fn runtime(&self, _service: Service) -> Option<Fact<Runtime>> {
        Some(NXKIT)
    }

    /// SDK 5: the address of the shared font is the TTF itself, whatever `main` imports.
    fn shared_font_decode(&self) -> Option<Fact<bool>> {
        Some(Fact::new(false, FONT_RULE))
    }
}

impl Title for Bastion {
    fn id(&self) -> TitleId {
        TitleId(0x0100_3860_0B27_E000)
    }

    fn name(&self) -> &'static str {
        "Bastion"
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
        let p = Profile::from_toml(BASTION.profile().unwrap_or_default()).unwrap();
        assert_eq!(p.title_id.as_deref(), Some(BASTION.id().to_string().as_str()));
        assert_eq!(p.declared.title.as_deref(), Some(BASTION.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = BASTION.standalone().unwrap();
        assert_eq!(s.version, 0);
        assert_eq!(s.modules.len(), 2);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.is_empty() || s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert_eq!(s.settings.value.display_versions[0].0, s.modules[0].1);
    }

    #[test]
    fn the_rules_hold_the_build_row() {
        let text = BASTION.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = BASTION.standalone().unwrap();
        let row = &t.gbuild["010038600B27E000"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 0);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| t.rules.get("010038600B27E000").and_then(|r| r.get(m)).map_or(0, Vec::len);
        assert_eq!(rules("sdk"), 12);
        assert_eq!(rules("main") + rules("subsdk0") + rules("subsdk1"), 0);
        assert!(t.frame.is_empty());
        assert_eq!(text.matches("evidence = \"engine tree ").count(), text.matches("[[rule]]").count());
    }

    #[test]
    fn nxkit_runs_every_service() {
        let on: Vec<Service> = Service::ALL
            .into_iter()
            .filter(|s| BASTION.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }
}
