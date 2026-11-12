//! The title module of Fire Emblem: Three Houses for Nintendo Switch (`010055D009F78000`).
//!
//! The game is update 1.2.0 (v327680) on SDK 6.5.0, graphics NVN. The engine tree names its engine: Koei Tecmo's KTGL.
//! No engine family of nxkit holds it. The modules are `main`, `subsdk0`, `subsdk1` and `sdk`. This module holds what
//! the engine tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v327680, the
//! shader split settings, the romfs marker and the run settings of the engine's `game.tsv`. The NVN function list is
//! not a hand list: the build reads the names from the modules' own strings (an empty list asks for that). All nine
//! services run on nxkit.
//!
//! The rewrite rules are data in `rules/010055D009F78000.toml`: the build row of v327680, 7 byte cuts of the game's own
//! code from `patches.py` and the engine's SDK-wide rules for the `sdk` module (12 implementations of the node area of
//! 62 MB and of the 2 MB alignment of large regions).

use nxtitle::{
    BuildHooks, Evidence, Fact, Memory, Runtime, RuntimeHooks, Service, Settings, Standalone, Title, TitleId,
};

/// The title module.
pub struct Fe3h;

/// The one instance the registry lists.
pub static FE3H: Fe3h = Fe3h;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/010055D009F78000/game.tsv and the defaults of dm/games/game.py at 034e91a8",
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
            ..Settings::DEFAULT.memory
        },
        display_versions: &[("89048449ba238c8cf565518b83bf02d3", "1.2.0")],
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

impl BuildHooks for Fe3h {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 327_680,
            modules: &[
                ("main", "89048449ba238c8cf565518b83bf02d3"),
                ("subsdk0", "8d073b25b9abda677e6967cef9ef6070"),
                ("subsdk1", "b9c00c0216af9b5b7f958ddbd25271c8"),
                ("sdk", "b2872a7bcc589969fb7afb8e372414e4"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "/DATA1.bin=data /patch4/=patch4 /patch3/=patch3 /patch2/=patch2 /patch1/=patch1 /nx/=nx \
                            /glslc_capture/=runtime",
            shader_groups_only: false,
            shader_note: "Fire Emblem: Three Houses shaders",
            shader_facts: None,
            romfs_marker: "/DATA1.bin",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/010055D009F78000.toml"))
    }
}

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A run of the game on the Mac checks each service.",
    },
);

impl RuntimeHooks for Fe3h {
    fn runtime(&self, _service: Service) -> Option<Fact<Runtime>> {
        Some(NXKIT)
    }

    /// SDK 6: the address of the shared font is the TTF itself, whatever `main` imports.
    fn shared_font_decode(&self) -> Option<Fact<bool>> {
        Some(Fact::new(false, FONT_RULE))
    }
}

impl Title for Fe3h {
    fn id(&self) -> TitleId {
        TitleId(0x0100_55D0_09F7_8000)
    }

    fn name(&self) -> &'static str {
        "Fire Emblem: Three Houses"
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
        let p = Profile::from_toml(FE3H.profile().unwrap_or_default()).unwrap();
        assert_eq!(p.title_id.as_deref(), Some(FE3H.id().to_string().as_str()));
        assert_eq!(p.declared.title.as_deref(), Some(FE3H.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = FE3H.standalone().unwrap();
        assert_eq!(s.version, 327_680);
        assert_eq!(s.modules.len(), 4);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.is_empty() || s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert_eq!(s.settings.value.display_versions[0].0, s.modules[0].1);
    }

    #[test]
    fn the_rules_hold_the_build_row() {
        let text = FE3H.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = FE3H.standalone().unwrap();
        let row = &t.gbuild["010055D009F78000"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 7);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| t.rules.get("010055D009F78000").and_then(|r| r.get(m)).map_or(0, Vec::len);
        assert_eq!(rules("sdk"), 12);
        assert_eq!(rules("main") + rules("subsdk0") + rules("subsdk1"), 0);
        assert!(t.frame.is_empty());
        assert_eq!(text.matches("evidence = \"engine tree ").count(), text.matches("[[rule]]").count());
    }

    #[test]
    fn nxkit_runs_every_service() {
        let on: Vec<Service> = Service::ALL
            .into_iter()
            .filter(|s| FE3H.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }
}
