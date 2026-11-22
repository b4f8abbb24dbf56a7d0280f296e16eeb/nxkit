//! The title module of OCTOPATH TRAVELER for Nintendo Switch (`010057D006492000`).
//!
//! The game is update 1.0.5 (v327680) on SDK 3.5.1, graphics NVN. The engine tree names its engine: Unreal Engine 4.18.
//! The family `unreal` holds it, and its frame limit rule writes the target into `t.MaxFPS`. The modules are `main`,
//! `subsdk0` and `sdk`. This module holds what the engine
//! tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v327680, the
//! shader split settings, the romfs marker and the run settings of the engine's `game.tsv`. The NVN function list is
//! not a hand list: the build reads the names from the modules' own strings (an empty list asks for that). All nine
//! services run on nxkit.
//!
//! The game ignores `-ExecCmds` (Shipping build). The module lists console variables ([`BuildHooks::console_vars`]), and
//! the family `unreal` writes them into an extra pak that the game mounts (`Kingship-Switch_1_P.pak` with an edited
//! `DefaultDeviceProfiles.ini`). They serve the MetalFX temporal upscaler: the game renders at half the back buffer
//! size per side and every frame is jittered.
//!
//! The module also holds what `settings.temporal_upscale` of `game.json` needs, and `upscaler()` is MetalFX: the encoder
//! replaces the game's TAA and upscale draws with the MetalFX temporal scaler. The draws are named by their fragment
//! shader keys ([`RuntimeHooks::temporal_draws`]). The layout of the view uniform buffer and of the velocity is the one of
//! UE 4.18's `FViewUniformShaderParameters`. It sits here and not in the family `unreal`, which has no engine version
//! and covers UE 4 and 5 ([`RuntimeHooks::temporal_view`]).
//!
//! The rewrite rules are data in `rules/010057D006492000.toml`: the build row of v327680 and the engine's SDK-wide
//! rules for the `sdk` module (12 implementations of the node area of 62 MB and of the 2 MB alignment of large
//! regions).

use nxtitle::{
    BuildHooks, ConfigKey, ConsoleVar, DrawSlot, Evidence, Fact, GraphicsApi, Memory, Runtime,
    RuntimeHooks, Service, Settings, SourceDraw, Standalone, TemporalDraws, TemporalView, Title,
    TitleId, UboStage, Upscaler, VelocityCode,
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

/// The highest frame rate the launcher offers. Unreal steps the game by the time that really passed. A run at a target
/// of 120 (`t.MaxFPS` 126, in Atlasdam about 80 fps, between 60 and 120 by the cost of the frames, menus at 120) walked,
/// animated and played its sound at the speed of a run at 60, judged by the player on 2026-11-22.
const FPS_MAX: Fact<u32> = Fact::new(
    120,
    Evidence::Recorded {
        source: "nxkit run of v327680 on an M2 Max with NXKIT_FPS_TARGET=120, the Metal HUD and NXKIT_DIAG=judder \
                 (run-fps-user120c.log); the speed judged against a run at 60 by the player",
        read: "2026-11-22",
    },
);

/// The default render size, 3840x2160. The pak of the console variables sets `r.ScreenPercentage=50` at build time, so
/// the game's own 1280x720 back buffer rendered the scene at 640x360. At 3840x2160 the scene is 1920x1080 and MetalFX
/// scales it to the back buffer. `NXKIT_RENDER_SIZE` still wins.
const RENDER_SIZE: Fact<u32> = Fact::new(
    2160,
    Evidence::Recorded {
        source: "nxkit runs of the temporal upscaling of 2026-11-22 (commit af12c951): a 3840x2160 back buffer with a \
                 1920x1080 scene, MetalFX output close to native 4K with the same colours",
        read: "2026-11-22",
    },
);

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
        // The game's config sets t.MaxFPS to 30. The limit rule of the family `unreal` writes the target into it.
        fps_target: 60,
        // Not from game.tsv: a run checked the game's speed above 60 ([`FPS_MAX`]).
        fps_max: FPS_MAX.value,
        // Not from game.tsv: the console variables of the pak render the scene at half the back buffer for MetalFX
        // ([`RENDER_SIZE`]).
        render_size: RENDER_SIZE.value,
        fps_cvar: Some("t.MaxFPS"),
        tex_flush_upload: true,
        tex_dirty_upload: true,
        pad_layout: "full",
        // CRIWARE ADX2 mixes 5.1 into a six-channel AudioOut in the SDK's order (L R C LFE Ls Rs), so the game gains
        // from spatial audio; the player can turn it off.
        spatial_audio: true,
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

/// A hand-made pak with these lines in `[Switch DeviceProfile]` took effect in a run on 2026-11-22: the scene rendered at
/// half size, and temporal anti-aliasing turned on.
const CVAR_RUN: Evidence = Evidence::Recorded {
    source: "manual run of OCTOPATH TRAVELER v327680 with Kingship/Content/Paks/Kingship-Switch_1_P.pak, \
             r.ScreenPercentage=50 and r.DefaultFeature.AntiAliasing=2",
    read: "2026-11-22",
};

/// The default sample count of the temporal upscaler is 8. The value is stated, so the pak does not depend on it.
const TAA_SAMPLES: Evidence = Evidence::Unverified {
    note: "8 is the engine's default for r.TemporalAASamples. A run that reads the console variable back checks that \
           the pak sets it.",
};

/// For the MetalFX temporal upscaler: `r.ScreenPercentage=50` renders at half the back buffer size per side,
/// `r.DefaultFeature.AntiAliasing=2` is temporal anti-aliasing, which jitters every frame, and
/// `r.TemporalAASamples=8` is the jitter sequence length.
const CONSOLE_VARS: &[Fact<ConsoleVar>] = &[
    Fact::new(
        ConsoleVar {
            name: "r.ScreenPercentage",
            value: "50",
        },
        CVAR_RUN,
    ),
    Fact::new(
        ConsoleVar {
            name: "r.DefaultFeature.AntiAliasing",
            value: "2",
        },
        CVAR_RUN,
    ),
    Fact::new(
        ConsoleVar {
            name: "r.TemporalAASamples",
            value: "8",
        },
        TAA_SAMPLES,
    ),
];

/// The game's `Kingship/Config/DefaultEngine.ini` scales its UI by `UIScaleCurve` over the shortest side with the keys
/// (50, 0), (720, 1) and (8640, 16): 1.68 at 1080 and 3.73 at 2160. The UI, laid out for 1280x720, was then wider than
/// the back buffer, too far right and cut off (raw frames at 1080 and 2160, 2026-11-22).
const UI_SCALE: Evidence = Evidence::Recorded {
    source: "Kingship/Config/DefaultEngine.ini of Kingship-Switch.pak v327680 ([/Script/Engine.UserInterfaceSettings]) \
             and raw frames (NXKIT_DIAG=shot) at 1920x1080 and 3840x2160",
    read: "2026-11-22",
};

/// The UI scale is the shortest side over 720 at any size, so the UI fills the back buffer as it does at 1280x720.
const CONFIG_KEYS: &[Fact<ConfigKey>] = &[Fact::new(
    ConfigKey {
        section: "/Script/Engine.UserInterfaceSettings",
        key: "UIScaleCurve",
        value: "(EditorCurveData=(PreInfinityExtrap=RCCE_Constant,PostInfinityExtrap=RCCE_Constant,\
                Keys=((Time=0.000000,Value=0.000000),(Time=720.000000,Value=1.000000),(Time=8640.000000,Value=12.000000)),\
                DefaultValue=340282346638528859811704183484516925440.000000),ExternalCurve=None)",
    },
    UI_SCALE,
)];

/// The view uniform buffer dumps and the bindings of the TAA draw in runs of the game.
const TAA_RUN: Evidence = Evidence::Recorded {
    source: "view uniform buffer dumps of OCTOPATH TRAVELER (NXKIT_DIAG=ubo with NXKIT_DIAG_FS) and the TAA draw's \
             bindings, runs of 2026-11-22",
    read: "2026-11-22",
};

/// The offsets belong to the layout of UE 4.18's `FViewUniformShaderParameters`. Another engine version may differ. The
/// velocity is a `Rg16Unorm` texel: UE writes `V * (0.499 * 0.5) + 32767 / 65535`, so `V = raw * 4.008016 - 2.003976`.
const TEMPORAL_VIEW: Fact<TemporalView> = Fact::new(
    TemporalView {
        clip_to_prev_clip: 1760,
        jitter: 1824,
        view_size: 1888,
        velocity: VelocityCode {
            scale: [4.008016, 4.008016],
            bias: [-2.003976, -2.003976],
            y_up: true,
            zero_means_camera: true,
        },
        depth_reversed: true,
    },
    TAA_RUN,
);

/// The fragment shaders of the TAA and upscale draws of v327680.
const TEMPORAL_DRAWS: Fact<TemporalDraws> = Fact::new(
    TemporalDraws {
        upscale: DrawSlot {
            fragment: "be89b1ada1308881",
            slot: 0,
        },
        source: SourceDraw {
            fragment: "cc0fa26ed24e4da1",
            depth_slot: 3,
            velocity_slot: 0,
            view_stage: UboStage::Fragment,
            view_slot: 0,
        },
        passthrough: &[
            DrawSlot {
                fragment: "cc0fa26ed24e4da1",
                slot: 2,
            },
            DrawSlot {
                fragment: "befb9b8e18bd3e4f",
                slot: 2,
            },
        ],
    },
    TAA_RUN,
);

impl BuildHooks for Octo {
    fn console_vars(&self) -> &'static [Fact<ConsoleVar>] {
        CONSOLE_VARS
    }

    fn config_keys(&self) -> &'static [Fact<ConfigKey>] {
        CONFIG_KEYS
    }

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

    /// MetalFX temporal upscaling replaces the game's TAA and upscale draws (R70, R72).
    fn upscaler(&self) -> Option<Fact<Upscaler>> {
        Some(Fact::new(Upscaler::MetalFx, TAA_RUN))
    }

    fn temporal_view(&self) -> Option<Fact<TemporalView>> {
        Some(TEMPORAL_VIEW)
    }

    fn temporal_draws(&self) -> Option<Fact<TemporalDraws>> {
        Some(TEMPORAL_DRAWS)
    }

    /// SDK 3: the address of the shared font is the TTF itself, whatever `main` imports.
    fn shared_font_decode(&self) -> Option<Fact<bool>> {
        Some(Fact::new(false, FONT_RULE))
    }
}

/// The graphics API of the game, as the games table of the engine tree names it.
const GRAPHICS: Fact<GraphicsApi> = Fact::new(
    GraphicsApi::Nvn,
    Evidence::Recorded {
        source: "engine tree dm/games/README.md at 131bd013: the Graphics column of the games table",
        read: "2026-11-22",
    },
);

impl Title for Octo {
    fn id(&self) -> TitleId {
        TitleId(0x0100_57D0_0649_2000)
    }

    fn name(&self) -> &'static str {
        "OCTOPATH TRAVELER"
    }

    fn engine(&self) -> &'static str {
        "unreal"
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
    fn the_ui_scale_is_the_shortest_side_over_720() {
        let [k] = OCTO.config_keys() else {
            panic!("one key")
        };
        assert_eq!(
            (k.value.section, k.value.key),
            ("/Script/Engine.UserInterfaceSettings", "UIScaleCurve")
        );
        let v = k.value.value;
        assert!(v.contains("(Time=720.000000,Value=1.000000),(Time=8640.000000,Value=12.000000)"));
        assert!(!v.contains(char::is_whitespace), "one ini line");
    }

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
    fn the_console_vars_serve_the_temporal_upscaler() {
        let vars: Vec<_> = OCTO
            .console_vars()
            .iter()
            .map(|f| (f.value.name, f.value.value))
            .collect();
        assert_eq!(
            vars,
            [
                ("r.ScreenPercentage", "50"),
                ("r.DefaultFeature.AntiAliasing", "2"),
                ("r.TemporalAASamples", "8")
            ]
        );
    }

    #[test]
    fn the_temporal_upscale_facts_are_the_recorded_ones() {
        let v = OCTO.temporal_view().unwrap().value;
        assert_eq!(
            [v.clip_to_prev_clip, v.jitter, v.view_size],
            [1760, 1824, 1888]
        );
        assert_eq!(v.velocity.scale, [4.008016; 2]);
        assert_eq!(v.velocity.bias, [-2.003976; 2]);
        assert!(v.velocity.y_up && v.velocity.zero_means_camera && v.depth_reversed);
        let d = OCTO.temporal_draws().unwrap().value;
        assert_eq!(
            (d.upscale.fragment, d.upscale.slot),
            ("be89b1ada1308881", 0)
        );
        assert_eq!(d.source.fragment, "cc0fa26ed24e4da1");
        assert_eq!((d.source.depth_slot, d.source.velocity_slot), (3, 0));
        assert_eq!(
            (d.source.view_stage, d.source.view_slot),
            (UboStage::Fragment, 0)
        );
        let pass: Vec<_> = d.passthrough.iter().map(|p| (p.fragment, p.slot)).collect();
        assert_eq!(pass, [("cc0fa26ed24e4da1", 2), ("befb9b8e18bd3e4f", 2)]);
        assert_eq!(OCTO.upscaler().map(|f| f.value), Some(Upscaler::MetalFx));
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
        let rules = |m: &str| {
            t.rules
                .get("010057D006492000")
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
            .filter(|s| OCTO.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
    }
}
