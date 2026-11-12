//! The title module of OCTOPATH TRAVELER for Nintendo Switch (`010057D006492000`).
//!
//! The game is update 1.0.5 (v327680) on SDK 3.5.1, graphics NVN. The engine tree names its engine: Unreal Engine 4.18.
//! No engine family of nxkit holds it. The modules are `main`, `subsdk0` and `sdk`. This module holds what the engine
//! tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v327680, the
//! shader split settings, the romfs marker and the run settings of the engine's `game.tsv`. The NVN function list is
//! not a hand list: the build reads the names from the modules' own strings (an empty list asks for that). All nine
//! services run on nxkit.
//!
//! The rewrite rules are data in `rules/010057D006492000.toml`: the build row of v327680 and the engine's SDK-wide
//! rules for the `sdk` module (12 implementations of the node area of 62 MB and of the 2 MB alignment of large
//! regions).

use nxtitle::{
    BuildHooks, Evidence, Fact, Memory, Runtime, RuntimeHooks, Service, Settings, Standalone, Title, TitleId,
};

/// The title module.
pub struct Octo;

/// The one instance the registry lists.
pub static OCTO: Octo = Octo;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/010057D006492000/game.tsv and the defaults of dm/games/game.py at 034e91a8",
    read: "2026-11-12",
};

const FONT_RULE: Evidence = Evidence::Recorded {
    source: "engine tree dm/host/hle_sdk16.c (font_skip) and dm/host/dm.h (DM_SDK_OLD) at 034e91a8",
    read: "2026-11-12",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The display
/// version is the one of the build row of v327680.
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0x1_0000_0000,
            alias: 0x1000_0000,
            stack: 0x1000_0000,
            app_pool: 0xCD50_0000,
            used: 0x800_0000,
            system_resource: 0,
            ..Settings::DEFAULT.memory
        },
        display_versions: &[("67dacc049ceee8585b1583336c565b43", "1.0.5")],
        fps_target: 60,
        fps_cvar: Some("t.MaxFPS"),
        tex_flush_upload: true,
        tex_dirty_upload: true,
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

impl BuildHooks for Octo {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 327_680,
            modules: &[
                ("main", "67dacc049ceee8585b1583336c565b43"),
                ("subsdk0", "798c30e126697f2222cf843f78f5c140"),
                ("sdk", "ae34e75d02925f4417b24499ad80c394"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "GlobalShaderCache=global #Engine/=engine #Kingship/=game",
            shader_groups_only: false,
            shader_note: "OCTOPATH TRAVELER shaders",
            shader_facts: None,
            romfs_marker: "/Kingship/Content/Paks/Kingship-Switch.pak",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/010057D006492000.toml"))
    }
}

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A run of the game on the Mac checks each service.",
    },
);

impl RuntimeHooks for Octo {
    fn runtime(&self, _service: Service) -> Option<Fact<Runtime>> {
        Some(NXKIT)
    }

    /// SDK 3: the address of the shared font is the TTF itself, whatever `main` imports.
    fn shared_font_decode(&self) -> Option<Fact<bool>> {
        Some(Fact::new(false, FONT_RULE))
    }
}

impl Title for Octo {
    fn id(&self) -> TitleId {
        TitleId(0x0100_57D0_0649_2000)
    }

    fn name(&self) -> &'static str {
        "OCTOPATH TRAVELER"
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
        let p = Profile::from_toml(OCTO.profile().unwrap_or_default()).unwrap();
        assert_eq!(p.title_id.as_deref(), Some(OCTO.id().to_string().as_str()));
        assert_eq!(p.declared.title.as_deref(), Some(OCTO.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = OCTO.standalone().unwrap();
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
        let text = OCTO.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = OCTO.standalone().unwrap();
        let row = &t.gbuild["010057D006492000"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 0);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| t.rules.get("010057D006492000").and_then(|r| r.get(m)).map_or(0, Vec::len);
        assert_eq!(rules("sdk"), 12);
        assert_eq!(rules("main") + rules("subsdk0") + rules("subsdk1"), 0);
        assert!(t.frame.is_empty());
        assert_eq!(text.matches("evidence = \"engine tree ").count(), text.matches("[[rule]]").count());
    }

    #[test]
    fn nxkit_runs_every_service() {
        let on: Vec<Service> = Service::ALL
            .into_iter()
            .filter(|s| OCTO.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }
}
