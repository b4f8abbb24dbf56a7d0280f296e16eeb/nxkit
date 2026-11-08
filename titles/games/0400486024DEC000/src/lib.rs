//! The title module of The Legend of Zelda: Ocarina of Time for Nintendo Switch 2 (`0400486024DEC000`).
//!
//! The game runs on Nintendo EPD's engine (family `agl`) on NVN2. What every game of that engine shares is in the
//! family. This module holds what is known to be true for this game. `render_cond`, `tex_flush_upload` and
//! `tex_dirty_upload` come from agl code, but Breath of the Wild does not set them, so they are here.
//!
//! The game builds without nxnative ([`BuildHooks::standalone`]): update v131072, SDK 22.2.8, the modules `main`,
//! `subsdk0` (the SDK's movie library) and `sdk`, read from a `.nup` container or from packages. The run settings are
//! the values of nxnative's `game.tsv`. The NVN function list is not a hand list: the build reads the names from the
//! modules' own strings (an empty list asks for that).
//!
//! The rewrite rules are data in `rules/0400486024DEC000.toml`: the frame ratio stub of nxnative's `patches.py`
//! (`MATCH_FRAME_STEP`), for main build `e0f3ccfaf50065ae4ea01eb5aeb13b1a` only. The game steps its time by a frame
//! ratio, so with the stub the host's frame step keeps game time equal to real time ([`Pacing::Display`]).
//!
//! The upscaler choice has no answer yet, so the core default applies. The seed list for the prepare step is
//! `pso_manifest.tsv` in `seedds/nxnative` `dm/games/0400486024DEC000/`.

use nxtitle::{
    BuildHooks, Evidence, Fact, Memory, Need, PaceProfile, Pacing, PresentInterval, Runtime, RuntimeHooks, Service,
    Settings, Standalone, Title, TitleId,
};

/// The fixed resolution scale (a resource patch at build time).
mod resolution;

/// The title module.
pub struct Oot;

/// The one instance the registry lists.
pub static OOT: Oot = Oot;

const README: Evidence = Evidence::Recorded {
    source: "seedds/nxnative dm/games/0400486024DEC000/README.md at c8bfa161",
    read: "2026-11-07",
};

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "seedds/nxnative dm/games/0400486024DEC000/game.tsv at 37f076a52168dadc86c679f483ae2e58519b3f7b",
    read: "2026-11-08",
};

const PATCHES_PY: Evidence = Evidence::Recorded {
    source: "seedds/nxnative dm/games/0400486024DEC000/patches.py at 37f076a52168dadc86c679f483ae2e58519b3f7b",
    read: "2026-11-08",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The memory
/// split is nxnative's starting split ("to be measured" in its comment): 256 MB in front, a 4 GB alias region, a
/// 256 MB stack region and the heap region, with the Switch 2's 9 GB application pool. 48 MB of system resource, as
/// the NPDM declares, so virtual address memory is on. The build row names no display version (`v131072` twice), so
/// the app shows the number of the update.
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0x3_2000_0000,
            alias: 0x1_0000_0000,
            stack: 0x1000_0000,
            app_pool: 0x2_4000_0000,
            system_resource: 0x300_0000,
            map_limit: true,
            ..Settings::DEFAULT.memory
        },
        fps_target: 60,
        pad_layout: "full",
        render_cond: true,
        gpu_ts_segment: true,
        draw_barriers_game: true,
        tex_flush_upload: true,
        tex_dirty_upload: true,
        ..Settings::DEFAULT
    },
    GAME_TSV,
);

/// No list here: the build reads the NVN function names from the modules (see the module comment).
const NVN_FUNCTIONS: Fact<&'static str> = Fact::new(
    "",
    Evidence::Unverified {
        note: "the build lists the NVN function names in the read-only data of the modules: what the game can fetch \
               through the NVN loader, a superset of what one play route calls. nxnative's usage.tsv of a route \
               checks it.",
    },
);

/// The game steps its time by a frame ratio (base rate / frame rate), and the frame ratio stub gives it the host's
/// step. With it the game runs at the display's rate and game time stays real time.
const PACING: Fact<Pacing> = Fact::new(Pacing::Display, PATCHES_PY);

/// `fps_target 60` of `game.tsv`. `fps_hold`, `present_snap` and `present_interval` are not set there, so they are
/// the defaults of `game.py` (on, off, game).
const PACE_PROFILE: Fact<PaceProfile> = Fact::new(
    PaceProfile {
        fps_target: 60,
        fps_hold: true,
        present_snap: false,
        present_interval: PresentInterval::Game,
    },
    GAME_TSV,
);

const NEEDS: &[Fact<Need>] = &[
    Fact::new(
        Need {
            capability: "gpu_timestamps",
            setting: "segment",
            why: "the game's dynamic resolution decides from GPU timestamps. With one time per submission it never \
                  lowers its resolution.",
        },
        README,
    ),
    Fact::new(
        Need {
            capability: "draw_barriers",
            setting: "game",
            why: "about 180 full barriers a frame that NVN does not ask for. GPU time 25 ms down to 18 ms at full \
                  resolution.",
        },
        README,
    ),
    Fact::new(
        Need {
            capability: "mem_map_limit",
            setting: "on",
            why: "the SDK allocator keeps freed pages mapped until a map fails, then gives them back. Without a \
                  failing map past the application pool the memory only grows.",
        },
        README,
    ),
    Fact::new(
        Need {
            capability: "render_cond",
            setting: "on",
            why: "agl clears a copy of the prepass depth again under a render condition the console skips. Run \
                  unconditionally, the opaque pass's EQUAL depth test fails everywhere.",
        },
        README,
    ),
    Fact::new(
        Need {
            capability: "tex_flush_upload",
            setting: "on",
            why: "the CPU writes terrain height, blend and hole maps into textures after creating them. Without \
                  the upload they stay zero: blue holes in the ground. Also seen in Tears of the Kingdom.",
        },
        README,
    ),
    Fact::new(
        Need {
            capability: "tex_dirty_upload",
            setting: "on",
            why: "the terrain maps are rewritten from the CPU as tile slots are reused. Also seen in Tears of the \
                  Kingdom.",
        },
        README,
    ),
];

impl BuildHooks for Oot {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 131_072,
            modules: &[
                ("main", "e0f3ccfaf50065ae4ea01eb5aeb13b1a"),
                ("subsdk0", "b0110087a7b339ef74588d3bd0b4dd5d"),
                ("sdk", "1643a838814f9c10c696f930ed681e7f"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            // the groups of nxnative's `game.tsv`: its split of the game found every file-sourced shader under them
            shader_groups: "/Shader/=shader /Effect/=effect /Lib/=lib /Layout2Shader/=layout /Movie/=movie",
            shader_groups_only: true,
            shader_note: "Ocarina of Time shaders",
            shader_facts: None,
            romfs_marker: "/Pack/ZsDic.pack.zs",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/0400486024DEC000.toml"))
    }

    fn resource_patches(&self) -> &'static [Fact<nxtitle::ResourcePatch>] {
        resolution::PATCHES
    }
}

/// The game builds without nxnative, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no nxnative path in a standalone build. A play of the title, the opening and Kokiri \
               Forest on the Mac checks each service.",
    },
);

impl RuntimeHooks for Oot {
    fn needs(&self) -> &'static [Fact<Need>] {
        NEEDS
    }

    fn runtime(&self, _service: Service) -> Option<Fact<Runtime>> {
        Some(NXKIT)
    }

    fn pacing(&self) -> Option<Fact<Pacing>> {
        Some(PACING)
    }

    fn pace_profile(&self) -> Option<Fact<PaceProfile>> {
        Some(PACE_PROFILE)
    }
}

impl Title for Oot {
    fn id(&self) -> TitleId {
        TitleId(0x0400_4860_24DE_C000)
    }

    fn name(&self) -> &'static str {
        "The Legend of Zelda: Ocarina of Time"
    }

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
        let p = Profile::from_toml(OOT.profile().unwrap_or_default()).unwrap();
        assert_eq!(p.title_id.as_deref(), Some(OOT.id().to_string().as_str()));
        assert_eq!(p.declared.title.as_deref(), Some(OOT.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn every_need_has_a_record() {
        assert!(OOT.needs().iter().all(|n| n.evidence.is_checked()));
        assert!(OOT.upscaler().is_none());
        assert!(OOT.pacing().is_some_and(|p| p.evidence.is_checked()));
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = OOT.standalone().unwrap();
        assert_eq!(s.version, 131_072);
        assert_eq!(s.modules.len(), 3);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        // an empty list: the build reads the names from the modules
        assert!(s.nvn_functions.value.is_empty());
        let m = s.settings.value.memory;
        assert_eq!((m.reserve, m.app_pool, m.system_resource), (0x3_2000_0000, 0x2_4000_0000, 0x300_0000));
        assert!(m.map_limit && s.settings.value.render_cond);
    }

    #[test]
    fn the_frame_step_rule_names_its_evidence() {
        let text = OOT.rewrite_rules().unwrap_or_default();
        assert!(text.contains("scope = \"0400486024DEC000\""));
        assert_eq!(text.matches("[[rule]]").count(), 1);
        assert_eq!(text.matches("kind = \"frame-step\"").count(), 1);
        assert_eq!(text.matches("evidence = \"seedds/nxnative ").count(), 1);
        assert!(text.contains("build = \"e0f3ccfaf50065ae4ea01eb5aeb13b1a\""));
    }

    #[test]
    fn nxkit_runs_every_service() {
        assert!(Service::ALL.into_iter().all(|s| OOT.runtime(s).map(|f| f.value) == Some(Runtime::Nxkit)));
    }
}
