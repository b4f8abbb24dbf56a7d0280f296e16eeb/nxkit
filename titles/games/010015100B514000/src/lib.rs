//! The title module of Super Mario Bros. Wonder for Nintendo Switch (`010015100B514000`).
//!
//! The game runs on Nintendo EPD's engine (family `agl`) on NVN, update 1.2.1 (v327680), SDK 16.2.12. The modules
//! are `main` and `sdk`. What every game of that engine shares is in the family. This module holds what is known to
//! be true for this game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v327680, the
//! shader split settings, the romfs marker and the run settings of the engine's `game.tsv`. The NVN function list is
//! not a hand list: the build reads the names from the modules' own strings (an empty list asks for that). All nine
//! services run on nxkit.
//!
//! The rewrite rules are data in `rules/010015100B514000.toml`: the build row of v327680 and the engine's two SDK-wide
//! rules for the `sdk` module, a node area of 62 MB in place of 1 GB and regions of 4 MB and more aligned to 2 MB. The
//! game's 2.875 GB alias region was measured with them. The engine has no code patch of the game's own `main`.
//!
//! The game sets none of the keys the family's other games need (`render_cond`, `tex_flush_upload`,
//! `tex_dirty_upload`, `pad_layout`): its `game.tsv` sets none of them, and the README says that a minute's boot
//! needed no render condition and that the virtual gamepad starts in the 2D layout.

use nxtitle::{
    BuildHooks, Evidence, Fact, Memory, Runtime, RuntimeHooks, Service, Settings, Standalone, Title, TitleId,
};

/// The title module.
pub struct Smbw;

/// The one instance the registry lists.
pub static SMBW: Smbw = Smbw;

const README: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/010015100B514000/README.md at b91cb749",
    read: "2026-11-12",
};

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/010015100B514000/game.tsv at b91cb749 and the dm/host/gen/010015100B514000/\
             game_profile.h that game.py generates from it",
    read: "2026-11-12",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The generated
/// `game_profile.h` has the defaults for every other key (`fps_target 0`, `fps_max 60`, `fps_hold 1`, no
/// snapshots, `sys_resource 0x2000000`). The memory layout is the README's 5 GB guest layout: 256 MB in front, a
/// 2.875 GB alias region, a 256 MB stack region and a 1.625 GB heap region. The game maps 1.75 GB of virtual address
/// memory (`System/HeapSize/*.HeapDef.byml`) on demand into the alias region and asks for a 1.47 GB heap; with a
/// smaller alias region large resources were skipped and a null dereference followed. The display version is the
/// one of the build row of v327680.
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0x1_4000_0000,
            alias: 0xB800_0000,
            stack: 0x1000_0000,
            app_pool: 0xCD50_0000,
            ..Settings::DEFAULT.memory
        },
        display_versions: &[("ff773e90972d544eb79406eaa65396d5", "1.2.1")],
        ..Settings::DEFAULT
    },
    GAME_TSV,
);

/// No list here: the build reads the NVN function names from the modules.
const NVN_FUNCTIONS: Fact<&'static str> = Fact::new(
    "",
    Evidence::Unverified {
        note: "the build lists the NVN function names in the read-only data of the modules: what the game can fetch \
               through the NVN loader. The engine's README counts 534 names that the loader fetches through \
               GetProcAddress, 236 of them called; a run that lists the fetched names checks the count.",
    },
);

impl BuildHooks for Smbw {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 327_680,
            modules: &[
                ("main", "ff773e90972d544eb79406eaa65396d5"),
                ("sdk", "e4bd58cac28cf13a08fb417aa013cdcb"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "/Shader/SecredUber=SecredUber .sharcb=agl /Effect/=effect /Layout/=layout \
                            /Shader/=SecredUber",
            shader_groups_only: false,
            shader_note: "Super Mario Bros. Wonder shaders",
            shader_facts: None,
            romfs_marker: "/System/HeapSize/Product.Nin_NX_NVN.HeapDef.byml.zs",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/010015100B514000.toml"))
    }
}

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A run to the opening course on the Mac checks \
               each service.",
    },
);

impl RuntimeHooks for Smbw {
    fn runtime(&self, _service: Service) -> Option<Fact<Runtime>> {
        Some(NXKIT)
    }

    /// SDK 16: `main` imports `ScalableFontEngineHelper::Decode`, so the shared font is the BFTTF form.
    fn shared_font_decode(&self) -> Option<Fact<bool>> {
        Some(Fact::new(true, README))
    }
}

impl Title for Smbw {
    fn id(&self) -> TitleId {
        TitleId(0x0100_1510_0B51_4000)
    }

    fn name(&self) -> &'static str {
        "Super Mario Bros. Wonder"
    }

    /// The README names the engine (sead, agl), and the RomFS holds `Lib/agl/agl_resource` and
    /// `Lib/gsys/gsys_resource`.
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

    #[test]
    fn profile_matches_the_module() {
        let p = Profile::from_toml(SMBW.profile().unwrap_or_default()).unwrap();
        assert_eq!(p.title_id.as_deref(), Some(SMBW.id().to_string().as_str()));
        assert_eq!(p.declared.title.as_deref(), Some(SMBW.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = SMBW.standalone().unwrap();
        assert_eq!(s.version, 327_680);
        assert_eq!(s.modules.len(), 2);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert_eq!(s.settings.value.memory.alias, 0xB800_0000);
        assert_eq!(s.settings.value.display_versions[0].0, s.modules[0].1);
    }

    #[test]
    fn the_rules_hold_the_build_row_and_the_sdk_rules() {
        let text = SMBW.rewrite_rules().unwrap();
        assert!(text.contains("scope = \"010015100B514000\""));
        let rules = text.matches("[[rule]]").count();
        assert_eq!(rules, 2);
        assert_eq!(text.matches("module = \"sdk\"").count(), rules);
        assert_eq!(text.matches("evidence = \"engine tree ").count(), rules);
        let s = SMBW.standalone().unwrap();
        assert!(text.contains(&format!("\"v{}\"", s.version)));
        for (name, id) in s.modules {
            assert!(text.contains(&format!("\"{name}={id}\"")), "{name}");
        }
    }

    #[test]
    fn nxkit_runs_every_service() {
        let on: Vec<Service> = Service::ALL
            .into_iter()
            .filter(|s| SMBW.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
        assert!(SMBW.shared_font_decode().is_some_and(|f| f.value && f.evidence.is_checked()));
    }
}
