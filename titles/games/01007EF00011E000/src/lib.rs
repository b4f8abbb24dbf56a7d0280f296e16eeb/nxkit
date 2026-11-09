//! The title module of The Legend of Zelda: Breath of the Wild for Nintendo Switch (`01007EF00011E000`).
//!
//! The game runs on Nintendo EPD's engine (family `agl`) on NVN, update 1.9.0 (v1114112), SDK 20.5.22. The
//! modules are `main`, `subsdk0` and `sdk`. What every game of that engine shares is in the family. This module
//! holds what is known to be true for this game.
//!
//! The game does not need `tex_flush_upload`, `render_cond` or `tex_dirty_upload`. The game's README in
//! The engine says so for `tex_flush_upload`, and its `game.tsv` sets none of the three.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the NVN function list, the
//! shader split settings, the two hand-written shader facts and the rewriter rules (`rewrite_rules`). All nine
//! services run on nxkit.
//!
//! The rewrite rules are data in `rules/01007EF00011E000.toml`: the seven interval controller cuts, the frame step
//! stubs, the render scale, render pool and output size stubs. Each rule names its source file, commit and read
//! date in its `evidence` key. The rules are the ones of `patches.py`.
//! They describe the build `cd57b23fa4bbad65803d9788c01821ee` of `main` and no other. The frame step protocol
//! (the state page the stubs read) is in `nxkit-svc-pacing`.
//!
//! The pacing keys of the game's profile (`fps_target`, `fps_hold`, `present_snap`, `present_interval`) are the fact
//! `PaceProfile`, with their evidence.

use nxkit_hos::{NameMatch, RoleRule, RoleTable, ThreadRole};
use nxtitle::{
    BuildHooks, Evidence, Fact, Memory, Need, PaceProfile, Pacing, PresentInterval, Runtime,
    RuntimeHooks, Service, Settings, Standalone, Title, TitleId,
};

/// The title module.
pub struct Botw;

/// The one instance the registry lists.
pub static BOTW: Botw = Botw;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/01007EF00011E000/game.tsv at f25fa956",
    read: "2026-11-07",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The generated
/// `game_profile.h` (`python3 dm/games/game.py header 01007EF00011E000 <out.h>`, read 2026-11-07) shows each result.
/// `render_base` (1600x900 docked, 1280x720 handheld) has no field: the host's resolution is its own business.
///
/// These are the engine's values, kept as the record of the game. nxkit's goals override some of them. R19 sets the
/// target to 120 fps through the frame step patch in the rewrite rules, so `fps_target 60` and `fps_max 60` do not cap
/// nxkit. `present_snap`, `enc_thread`, `core_preempt`, `draw_barriers_game` and the `tex_*` keys describe the engine's
/// translator. R10 and R17 replace them (encoding on its own thread, a whole frame, barriers only where data flows).
/// The host reader of `game.json` (`nxkit-host`) reads no `settings` object yet, checked on 2026-11-07.
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0x1_0000_0000,
            alias: 0x1000_0000,
            stack: 0x1000_0000,
            app_pool: 0xCD50_0000,
            ..Settings::DEFAULT.memory
        },
        display_versions: &[("cd57b23fa4bbad65803d9788c01821ee", "1.9.0")],
        fps_target: 60,
        present_snap: true,
        pso_mtl4: true,
        screen_aspect: true,
        pad_layout: "full",
        gpu_ts_segment: true,
        ..Settings::DEFAULT
    },
    GAME_TSV,
);

const NEEDS: &[Fact<Need>] = &[
    Fact::new(
        Need {
            capability: "gpu_timestamps",
            setting: "segment",
            why: "the frame interval controller and the dynamic resolution read the GPU time between the frame's two \
                  timestamp reports. Each report takes its segment's GPU start or end.",
        },
        GAME_TSV,
    ),
    Fact::new(
        Need {
            capability: "fps_target",
            setting: "60",
            why: "the host picks each frame's step in whole vsyncs, so the game runs at 60 fps (30 on the Switch). \
                  This needs the frame step stubs in rules/01007EF00011E000.toml.",
        },
        GAME_TSV,
    ),
    Fact::new(
        Need {
            capability: "present_snap",
            setting: "on",
            why: "present snapshots keep the GPU busy between frames. Without them iOS lowers the GPU clock and the \
                  game holds 30 fps instead of 60 (iPhone 17 Pro Max, GitHub issues #142 and #159).",
        },
        GAME_TSV,
    ),
    Fact::new(
        Need {
            capability: "pso_mtl4",
            setting: "on",
            why: "render pipelines through MTL4Compiler give the pipeline archive and the prepare step on the \
                  iPhone too. 5,100 prewarm lines are prepared in 0.28 s at start.",
        },
        GAME_TSV,
    ),
    Fact::new(
        Need {
            capability: "screen_aspect",
            setting: "on",
            why: "a screen taller than 16:9 gets a picture of its own shape. The launcher offers the setting only \
                  with this key.",
        },
        GAME_TSV,
    ),
];

const USAGE: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/01007EF00011E000/usage.tsv at 88c7e03f (the nvn rows, route smoke.tsv quick, 70 s)",
    read: "2026-11-07",
};

/// The NVN functions of one 70 s route are a lower bound. A function the route did not reach is missing. Two of the
/// names (`nvnCommandBufferSetDepthBounds` and `nvnCommandBufferSetShaderScratchMemory`) are not in the engine's
/// implemented list.
const NVN_FUNCTIONS: Fact<&'static str> = Fact::new(include_str!("../nvn_functions.txt"), USAGE);

const PATCHES_PY: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/01007EF00011E000/patches.py at \
             0f2a772c13e2caed2959d1d1a93f8fe57f44c754",
    read: "2026-11-07",
};

/// The game steps its logic once per interval of the framework, and the frame step stubs let the host choose the
/// interval. With them the game runs at the display's rate. The record is the pull request's own description.
const PACING: Fact<Pacing> = Fact::new(Pacing::Display, PATCHES_PY);

const PROFILE: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/01007EF00011E000/game.tsv, dm/games/game.py (defaults) and \
             dm/host/gen/01007EF00011E000/game_profile.h at 88c7e03f",
    read: "2026-11-07",
};

/// The keys of the game's profile in the engine that steer the frame step and the presents. `game.tsv` sets `fps_target 60`
/// and `present_snap on`. `fps_hold` (on) and `present_interval` (game) are the defaults of `game.py`, and
/// `game_profile.h` has them as `DM_GAME_FPS_HOLD 1` and `DM_GAME_PRESENT_DISPLAY 0`.
///
/// With the frame step the hold of each frame overrides `present_interval`, so `Game` only decides a frame that no cap
/// covers. The generated header of that checkout has no `fps_max` line, so the maximum stays at the default of 60 and
/// this game never runs above 60 through the limiter path.
const PACE_PROFILE: Fact<PaceProfile> = Fact::new(
    PaceProfile {
        fps_target: 60,
        fps_hold: true,
        present_snap: true,
        present_interval: PresentInterval::Game,
    },
    PROFILE,
);

impl BuildHooks for Botw {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 1_114_112,
            modules: &[
                ("main", "cd57b23fa4bbad65803d9788c01821ee"),
                ("subsdk0", "f39f4c431234c72eb520cfdd1001bbcf"),
                ("sdk", "574284b40c06cf1821a44857939083be"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "/Shader/=shader /Effect/=effect /Layout/=layout /System/=gsys /Terrain/=terrain",
            shader_groups_only: false,
            shader_note: "Breath of the Wild shaders",
            shader_facts: Some(include_str!("../shader_facts.tsv")),
            romfs_marker: "/Pack/Bootup_Graphics.pack",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/01007EF00011E000.toml"))
    }
}

const THREAD_NOTE: &str = "the thread names and priorities are guesses from the engine's usual layout and are not \
                           read from the game. Checked on 2026-11-07 and nothing found: the game folder holds NSPs and no \
                           extracted main NSO, and the engine logs and notes name no BotW thread (only that the main thread \
                           and two workers on cores 1 and 2 poll one job queue). A thread dump of a run (nxkit-hos \
                           `dump_stats`) or the strings of the extracted main NSO checks them.";

/// The roles of the game's threads. Every entry is unverified.
pub const THREAD_ROLES: &[Fact<RoleRule>] = &[
    Fact::new(
        RoleRule::named(NameMatch::Contains("Render"), ThreadRole::Render),
        Evidence::Unverified { note: THREAD_NOTE },
    ),
    Fact::new(
        RoleRule::named(NameMatch::Contains("Audio"), ThreadRole::Audio),
        Evidence::Unverified { note: THREAD_NOTE },
    ),
    Fact::new(
        RoleRule::named(NameMatch::Contains("Load"), ThreadRole::Loading),
        Evidence::Unverified { note: THREAD_NOTE },
    ),
    Fact::new(
        RoleRule::named(NameMatch::Contains("Stream"), ThreadRole::Streaming),
        Evidence::Unverified { note: THREAD_NOTE },
    ),
];

/// The table the kernel takes for this title. A thread no entry matches keeps the default role.
pub fn role_table() -> RoleTable {
    RoleTable::new(&THREAD_ROLES.iter().map(|f| f.value).collect::<Vec<_>>())
}

/// The game builds without the engine, so nxkit runs every service. The services' answers follow the engine's
/// (`dm/host/hle_*.c`). The nvn rows and the hle rows of `usage.tsv` list what the game calls.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A play of the fixed route on the Mac checks \
              each service.",
    },
);

const SHARED_FONT: Evidence = Evidence::Recorded {
    source: "engine tree dm/host/hle_sdk16.c (font_skip comment) at f25fa956",
    read: "2026-11-07",
};

impl RuntimeHooks for Botw {
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

    fn shared_font_decode(&self) -> Option<Fact<bool>> {
        Some(Fact::new(true, SHARED_FONT))
    }
}

impl Title for Botw {
    fn id(&self) -> TitleId {
        TitleId(0x0100_7EF0_0011_E000)
    }

    fn name(&self) -> &'static str {
        "The Legend of Zelda: Breath of the Wild"
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
        let p = Profile::from_toml(BOTW.profile().unwrap_or_default()).unwrap();
        assert_eq!(p.title_id.as_deref(), Some(BOTW.id().to_string().as_str()));
        assert_eq!(p.declared.title.as_deref(), Some(BOTW.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn thread_roles_are_marked_unverified() {
        assert!(THREAD_ROLES.iter().all(|r| !r.evidence.is_checked()));
        let info = nxkit_hos::ThreadInfo {
            name: "AudioMain",
            priority: 20,
            entry: 0,
        };
        assert_eq!(role_table().classify(&info), Some(ThreadRole::Audio));
    }

    #[test]
    fn every_need_has_a_record() {
        assert!(BOTW.needs().iter().all(|n| n.evidence.is_checked()));
        assert!(BOTW.upscaler().is_none());
        assert!(BOTW.pacing().is_some_and(|p| p.evidence.is_checked()));
    }

    #[test]
    fn the_pace_profile_is_the_one_of_the_game_profile() {
        let Some(p) = BOTW.pace_profile() else {
            return;
        };
        assert!(p.evidence.is_checked());
        assert_eq!(p.value.fps_target, 60);
        assert!(p.value.fps_hold && p.value.present_snap);
        assert_eq!(p.value.present_interval, PresentInterval::Game);
    }

    #[test]
    fn every_rewrite_rule_names_its_evidence() {
        let text = BOTW.rewrite_rules().unwrap_or_default();
        let rules = text.matches("[[rule]]").count();
        assert_eq!(rules, 8);
        assert_eq!(text.matches("evidence = \"engine tree ").count(), rules);
        assert!(text.contains("read 2026-11-07"));
    }

    #[test]
    fn nxkit_runs_every_service() {
        let on: Vec<Service> = Service::ALL
            .into_iter()
            .filter(|s| BOTW.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = BOTW.standalone().unwrap();
        let names: Vec<&str> = s.nvn_functions.value.lines().collect();
        assert_eq!(names.len(), 197);
        assert!(names.iter().all(|n| n.starts_with("nvn")));
        assert!(names.contains(&"nvnDeviceInitialize"));
        assert_eq!(s.modules.len(), 3);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.evidence.is_checked());
        assert!(
            BOTW.rewrite_rules()
                .unwrap()
                .contains("scope = \"01007EF00011E000\"")
        );
    }
}
