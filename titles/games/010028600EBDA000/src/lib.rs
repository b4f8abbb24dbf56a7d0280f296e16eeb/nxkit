//! The title module of Super Mario 3D World + Bowser's Fury for Nintendo Switch (`010028600EBDA000`).
//!
//! The game is update 1.2.2 (v262144) on SDK 10.4.0, graphics NVN. The engine tree names its engine: Nintendo EPD (sead
//! / agl, NintendoWare). What every game of that engine shares is in the family `agl`. The modules are `main` and
//! `sdk`. This module holds what the engine tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v262144, the
//! shader split settings, the romfs marker and the run settings of the engine's `game.tsv`. The NVN function list is
//! not a hand list: the build reads the names from the modules' own strings (an empty list asks for that). All nine
//! services run on nxkit.
//!
//! The rewrite rules are data in `rules/010028600EBDA000.toml`: the build row of v262144 and the engine's SDK-wide
//! rules for the `sdk` module (12 implementations of the node area of 62 MB and of the 2 MB alignment of large
//! regions).

use nxtitle::{
    BuildHooks, Evidence, Fact, Memory, Runtime, RuntimeHooks, Service, Settings, Standalone, Title, TitleId,
};

/// The title module.
pub struct Sm3dwbf;

/// The one instance the registry lists.
pub static SM3DWBF: Sm3dwbf = Sm3dwbf;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/010028600EBDA000/game.tsv and the defaults of dm/games/game.py at 034e91a8",
    read: "2026-11-12",
};

const FONT_RULE: Evidence = Evidence::Recorded {
    source: "engine tree dm/host/hle_sdk16.c (font_skip) and dm/host/dm.h (DM_SDK_OLD) at 034e91a8",
    read: "2026-11-12",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The display
/// version is the one of the build row of v262144.
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0x1_0000_0000,
            alias: 0x1000_0000,
            stack: 0x1000_0000,
            app_pool: 0xCD50_0000,
            ..Settings::DEFAULT.memory
        },
        display_versions: &[("635fe25c7cb69847f41b49f19cd9aa89", "1.2.2")],
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

impl BuildHooks for Sm3dwbf {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 262_144,
            modules: &[
                ("main", "635fe25c7cb69847f41b49f19cd9aa89"),
                ("sdk", "204eaef83fe5e5617d0787c8ef01a080"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "/ShaderData/=shader /EffectData/=effect /LayoutData/=layout /SystemData/=agl",
            shader_groups_only: false,
            shader_note: "Super Mario 3D World + Bowser's Fury shaders",
            shader_facts: None,
            romfs_marker: "/ShaderData/RenderMaterial.szs",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/010028600EBDA000.toml"))
    }
}

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A run of the game on the Mac checks each service.",
    },
);

impl RuntimeHooks for Sm3dwbf {
    fn runtime(&self, _service: Service) -> Option<Fact<Runtime>> {
        Some(NXKIT)
    }

    /// SDK 10: the address of the shared font is the TTF itself, whatever `main` imports.
    fn shared_font_decode(&self) -> Option<Fact<bool>> {
        Some(Fact::new(false, FONT_RULE))
    }
}

impl Title for Sm3dwbf {
    fn id(&self) -> TitleId {
        TitleId(0x0100_2860_0EBD_A000)
    }

    fn name(&self) -> &'static str {
        "Super Mario 3D World + Bowser's Fury"
    }

    /// The engine tree names the engine: Nintendo EPD (sead / agl, NintendoWare).
    fn family(&self) -> Option<&'static str> {
        Some("agl")
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
        let p = Profile::from_toml(SM3DWBF.profile().unwrap_or_default()).unwrap();
        assert_eq!(p.title_id.as_deref(), Some(SM3DWBF.id().to_string().as_str()));
        assert_eq!(p.declared.title.as_deref(), Some(SM3DWBF.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = SM3DWBF.standalone().unwrap();
        assert_eq!(s.version, 262_144);
        assert_eq!(s.modules.len(), 2);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.is_empty() || s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert_eq!(s.settings.value.display_versions[0].0, s.modules[0].1);
    }

    #[test]
    fn the_rules_hold_the_build_row() {
        let text = SM3DWBF.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = SM3DWBF.standalone().unwrap();
        let row = &t.gbuild["010028600EBDA000"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 0);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| t.rules.get("010028600EBDA000").and_then(|r| r.get(m)).map_or(0, Vec::len);
        assert_eq!(rules("sdk"), 12);
        assert_eq!(rules("main") + rules("subsdk0") + rules("subsdk1"), 0);
        assert!(t.frame.is_empty());
        assert_eq!(text.matches("evidence = \"engine tree ").count(), text.matches("[[rule]]").count());
    }

    #[test]
    fn nxkit_runs_every_service() {
        let on: Vec<Service> = Service::ALL
            .into_iter()
            .filter(|s| SM3DWBF.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }
}
