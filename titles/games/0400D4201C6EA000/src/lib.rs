//! The title module of Mario Tennis Fever for Nintendo Switch 2 (`0400D4201C6EA000`).
//!
//! The game is version v327680 on SDK 20.5.17 (Ounce), graphics NVN2. The engine tree names its engine: Camelot's
//! engine, NintendoWare. No engine family of nxkit holds it. The modules are `main` and `sdk`. This module holds what
//! the engine tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v327680, the
//! shader split settings, the romfs marker and the run settings of the engine's `game.tsv`. The NVN function list is
//! not a hand list: the build reads the names from the modules' own strings (an empty list asks for that). All nine
//! services run on nxkit.
//!
//! The rewrite rules are data in `rules/0400D4201C6EA000.toml`: the build row of v327680 and the engine's SDK-wide
//! rules for the `sdk` module (12 implementations of the node area of 62 MB and of the 2 MB alignment of large
//! regions).
//!
//! The game loads NRO modules at run time through `nn::ro`. nxkit takes the calls over (`nxkit-svc-system`,
//! `sdk/ro.rs`); a run of this game has not checked it.
//!
//! The form of the shared font is left to the core default (the TTF itself): on SDK 20 the engine serves the BFTTF form
//! when `main` imports `ScalableFontEngineHelper::Decode`, and nobody checked the imports of this game.
//!
//! The first fact of the module is a command buffer quirk (`vertex_stream_null_bind`).

use nxtitle::{
    BuildHooks, Evidence, Fact, Memory, Need, Runtime, RuntimeHooks, Service, Settings, Standalone,
    Title, TitleId,
};

/// The title module.
pub struct Mtf;

/// The one instance the registry lists.
pub static MTF: Mtf = Mtf;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/0400D4201C6EA000/game.tsv and the defaults of dm/games/game.py at 034e91a8",
    read: "2026-11-12",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The display
/// version is the one of the build row of v327680.
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0x6_5000_0000,
            alias: 0x5_0000_0000,
            stack: 0x1000_0000,
            app_pool: 0x2_4000_0000,
            ..Settings::DEFAULT.memory
        },
        display_versions: &[("104bce6874603701935eb76cb7d8b59e", "v327680")],
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

impl BuildHooks for Mtf {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 327_680,
            modules: &[
                ("main", "104bce6874603701935eb76cb7d8b59e"),
                ("sdk", "32a9ac1480a6f0e9e2e8a813ae9cd731"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "/shader/=shader /model/=model /effect/=effect /layout/=layout /ssg_system/=system",
            shader_groups_only: false,
            shader_note: "Mario Tennis Fever shaders",
            shader_facts: None,
            romfs_marker: "/42",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/0400D4201C6EA000.toml"))
    }
}

const HLE_NVN: Evidence = Evidence::Recorded {
    source: "engine tree dm/gfx/nvn/hle_nvn.c at a9d06b6f, nvnCommandBufferBindVertexBuffers",
    read: "2026-11-07",
};

const NEEDS: &[Fact<Need>] = &[Fact::new(
    Need {
        capability: "vertex_stream_null_bind",
        setting: "ignore",
        why: "the game binds a null range to the vertex streams 16 to 31 before each model, thousands of times a run. \
              NVN has 16 streams, so nothing reads them. The command buffer ignores a bind of one null range at \
              the stream 16 or above. A real range past stream 15 still ends the game.",
    },
    HLE_NVN,
)];

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A run of the game on the Mac checks each service.",
    },
);

impl RuntimeHooks for Mtf {
    fn runtime(&self, _service: Service) -> Option<Fact<Runtime>> {
        Some(NXKIT)
    }

    fn needs(&self) -> &'static [Fact<Need>] {
        NEEDS
    }
}

impl Title for Mtf {
    fn id(&self) -> TitleId {
        TitleId(0x0400_D420_1C6E_A000)
    }

    fn name(&self) -> &'static str {
        "Mario Tennis Fever"
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
        let p = Profile::from_toml(MTF.profile().unwrap_or_default()).unwrap();
        assert_eq!(p.title_id.as_deref(), Some(MTF.id().to_string().as_str()));
        assert_eq!(p.declared.title.as_deref(), Some(MTF.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = MTF.standalone().unwrap();
        assert_eq!(s.version, 327_680);
        assert_eq!(s.modules.len(), 2);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.is_empty() || s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert_eq!(s.settings.value.display_versions[0].0, s.modules[0].1);
    }

    #[test]
    fn the_rules_hold_the_build_row() {
        let text = MTF.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = MTF.standalone().unwrap();
        let row = &t.gbuild["0400D4201C6EA000"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 0);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| {
            t.rules
                .get("0400D4201C6EA000")
                .and_then(|r| r.get(m))
                .map_or(0, Vec::len)
        };
        assert_eq!(rules("sdk"), 12);
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
            .filter(|s| MTF.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }

    #[test]
    fn the_command_buffer_need_has_a_record() {
        let keys: Vec<_> = MTF.needs().iter().map(|n| n.value.capability).collect();
        assert_eq!(keys, ["vertex_stream_null_bind"]);
        assert!(MTF.needs().iter().all(|n| n.evidence.is_checked()));
    }
}
