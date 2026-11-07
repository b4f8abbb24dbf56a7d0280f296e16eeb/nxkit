//! The title module of The Legend of Zelda: Breath of the Wild for Nintendo Switch (`01007EF00011E000`).
//!
//! The game runs on Nintendo EPD's engine (family `agl`) on NVN, update 1.9.0 (v1114112), SDK 20.5.22. The
//! modules are `main`, `subsdk0` and `sdk`. What every game of that engine shares is in the family. This module
//! holds what is known to be true for this game.
//!
//! The game does not need `tex_flush_upload`, `render_cond` or `tex_dirty_upload`. The game's README in
//! nxnative says so for `tex_flush_upload`, and its `game.tsv` sets none of the three.
//!
//! These parts are in `seedds/nxnative` `dm/games/01007EF00011E000/` and not in this module:
//!
//! - `patches.py`: the `FRAME_STEP` stubs for `fps_target`, the interval controller cuts (`CUTS`) and the render
//!   scale stubs.
//! - `shader_facts_manual.tsv`: two shaders with integer outputs from agl's BC4 encoder.
//!
//! nxnative runs every service of the game. This module moves a service to nxkit once the nxkit runtime has it.

use nxkit_hos::{NameMatch, RoleRule, RoleTable, ThreadRole};
use nxtitle::{BuildHooks, Evidence, Fact, Need, Runtime, RuntimeHooks, Service, Title, TitleId};

/// The title module.
pub struct Botw;

/// The one instance the registry lists.
pub static BOTW: Botw = Botw;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "seedds/nxnative dm/games/01007EF00011E000/game.tsv at f25fa956",
    read: "2026-11-07",
};

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
                  This needs the FRAME_STEP stubs from patches.py.",
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

impl BuildHooks for Botw {}

const THREAD_NOTE: &str = "the thread names and priorities are guesses from the engine's usual layout and are not \
                           read from the game. A thread dump of a run (nxkit-hos `dump_stats`) checks them.";

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

/// The services nxkit runs for Breath of the Wild. The rest stay on nxnative.
const OFFLINE: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "nxkit-rt answers the offline entries as hle_offline.c and hle_net.c do (nxkit_rt::offline tests). \
              A play of the fixed route on the Mac checks it.",
    },
);

const SHARED_FONT: Evidence = Evidence::Recorded {
    source: "seedds/nxnative dm/host/hle_sdk16.c (font_skip comment) at f25fa956",
    read: "2026-11-07",
};

impl RuntimeHooks for Botw {
    fn needs(&self) -> &'static [Fact<Need>] {
        NEEDS
    }

    fn runtime(&self, service: Service) -> Option<Fact<Runtime>> {
        (service == Service::Offline).then_some(OFFLINE)
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
        assert!(BOTW.upscaler().is_none() && BOTW.pacing().is_none());
    }

    #[test]
    fn nxkit_runs_only_offline() {
        let on: Vec<Service> = Service::ALL
            .into_iter()
            .filter(|s| BOTW.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, [Service::Offline]);
    }
}
