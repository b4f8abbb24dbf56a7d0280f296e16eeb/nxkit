//! The title module of The Legend of Zelda: Ocarina of Time for Nintendo Switch 2 (`0400486024DEC000`).
//!
//! The game runs on Nintendo EPD's engine (family `agl`) on NVN2. What every game of that engine shares is in the
//! family. This module holds what is known to be true for this game. `render_cond`, `tex_flush_upload` and
//! `tex_dirty_upload` come from agl code, but Breath of the Wild does not set them, so they are here.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): update v131072, SDK 22.2.8, the modules `main`,
//! `subsdk0` (the SDK's movie library) and `sdk`, read from a `.nup` container or from packages. The run settings are
//! the values of the engine's `game.tsv`. The NVN function list is not a hand list: the build reads the names from the
//! modules' own strings (an empty list asks for that).
//!
//! The rewrite rules are data in `rules/0400486024DEC000.toml`: the frame ratio stub of the engine's `patches.py`
//! (`MATCH_FRAME_STEP`), for main build `e0f3ccfaf50065ae4ea01eb5aeb13b1a` only. The game steps its time by a frame
//! ratio, so with the stub the host's frame step keeps game time equal to real time ([`Pacing::Display`]). The stub
//! follows the host's step in both rates: the whole step in vsyncs of 1/60 s at a target of 60, and at 120 the time step
//! (1/30 s units) as twice its value in vsyncs, so the ratio is half per 120 Hz tick. The settings say `fps_target 120`
//! and `fps_max 120`.
//!
//! The upscaler choice has no answer yet, so the core default applies. The seed list for the prepare step is
//! `pso_manifest.tsv` in the engine tree `dm/games/0400486024DEC000/`.

use nxtitle::{
    BuildHooks, Evidence, Fact, Memory, Need, PaceProfile, Pacing, PresentInterval, Runtime,
    RuntimeHooks, Service, Settings, Standalone, Title, TitleId,
};

/// The fixed resolution scale (a resource patch at build time).
mod resolution;

/// The title module.
pub struct Oot;

/// The one instance the registry lists.
pub static OOT: Oot = Oot;

const README: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/0400486024DEC000/README.md at c8bfa161",
    read: "2026-11-07",
};

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/0400486024DEC000/game.tsv at 37f076a52168dadc86c679f483ae2e58519b3f7b",
    read: "2026-11-08",
};

const PATCHES_PY: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/0400486024DEC000/patches.py at 37f076a52168dadc86c679f483ae2e58519b3f7b",
    read: "2026-11-08",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The memory
/// split is the engine's starting split ("to be measured" in its comment): 256 MB in front, a 4 GB alias region, a
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
        fps_target: 120,
        fps_max: 120,
        pad_layout: "full",
        render_cond: true,
        gpu_ts_segment: true,
        draw_barriers_game: true,
        tex_flush_upload: true,
        tex_dirty_upload: true,
        // Measured on nxkit: without it the main thread waited about three quarters of a frame for the GPU in
        // nvnSyncWait and the opening movie fell to 18-30 fps; with it the opening holds 60.
        overlap: true,
        ..Settings::DEFAULT
    },
    GAME_TSV,
);

/// No list here: the build reads the NVN function names from the modules (see the module comment).
const NVN_FUNCTIONS: Fact<&'static str> = Fact::new(
    "",
    Evidence::Unverified {
        note: "the build lists the NVN function names in the read-only data of the modules: what the game can fetch \
               through the NVN loader, a superset of what one play route calls. The engine's usage.tsv of a route \
               checks it.",
    },
);

/// The game steps its time by a frame ratio (base rate / frame rate), and the frame ratio stub gives it the host's
/// step. With it the game runs at the display's rate and game time stays real time.
const PACING: Fact<Pacing> = Fact::new(Pacing::Display, PATCHES_PY);

/// `fps_target 120` with `fps_max 120`. `game.tsv` says 60. At 120 the host steps by time (the variable step of the state
/// page) and the frame ratio stub turns it into the ratio, so game time stays real time (see the rules file). The speed
/// at 120 fps is checked in the game, not yet: a 10 s cutscene must take 10 s. `fps_hold`, `present_snap` and `present_interval` are not set there, so they are
/// the defaults of `game.py` (on, off, game).
const PACE_PROFILE: Fact<PaceProfile> = Fact::new(
    PaceProfile {
        fps_target: 120,
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
            // the groups of the engine's `game.tsv`: its split of the game found every file-sourced shader under them
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

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A play of the title, the opening and Kokiri \
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
    use nxb_a64::decode;
    use nxb_profile::Profile;
    use nxb_rewrite::patches::{FsOp, PatchTable};

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
        assert_eq!(
            (m.reserve, m.app_pool, m.system_resource),
            (0x3_2000_0000, 0x2_4000_0000, 0x300_0000)
        );
        assert!(m.map_limit && s.settings.value.render_cond);
    }

    #[test]
    fn the_frame_step_rule_names_its_evidence() {
        let text = OOT.rewrite_rules().unwrap_or_default();
        assert!(text.contains("scope = \"0400486024DEC000\""));
        assert_eq!(text.matches("[[rule]]").count(), 1);
        assert_eq!(text.matches("kind = \"frame-step\"").count(), 1);
        assert_eq!(text.matches("evidence = \"engine tree ").count(), 1);
        assert!(text.contains("build = \"e0f3ccfaf50065ae4ea01eb5aeb13b1a\""));
    }

    /// A word of the stub, for the model below.
    enum Item {
        Word(u32),
        Adrp(usize),
        Exit,
    }

    fn stub() -> Vec<Item> {
        let files = [("o.toml".to_owned(), OOT.rewrite_rules().unwrap().to_owned())];
        let t = PatchTable::from_toml(&files).unwrap();
        t.frame_ops["e0f3ccfaf50065ae4ea01eb5aeb13b1a"]
            .iter()
            .filter_map(|o| match o {
                FsOp::Word(w) => Some(Item::Word(*w)),
                FsOp::Adrp(r, ..) => Some(Item::Adrp(*r as usize)),
                FsOp::B(..) => Some(Item::Exit),
                _ => None,
            })
            .collect()
    }

    /// The game's frame rate in `s0` and the base rate in `s1` when the stub runs.
    const GAME_RATE: f32 = 45.0;
    const BASE_RATE: f32 = 60.0;

    /// Runs the stub on a state page (`(offset, word)` pairs, the rest is 0) and returns `s8` and the stepped count at
    /// `+0x30`. Only the instructions of the stub are modelled, and `nxb_a64` decodes each one first.
    fn run(page: &[(u64, u32)]) -> (f32, u32) {
        let items = stub();
        let mut mem: std::collections::HashMap<u64, u32> = page.iter().copied().collect();
        let (mut x, mut s) = ([0u64; 32], [0f32; 32]);
        (s[0], s[1]) = (GAME_RATE, BASE_RATE);
        let (mut pc, mut z) = (0usize, false);
        for _ in 0..64 {
            let w = match items.get(pc) {
                Some(Item::Word(w)) => *w,
                Some(Item::Adrp(r)) => {
                    x[*r] = 0; // the page base: offsets are the addresses
                    pc += 1;
                    continue;
                }
                Some(Item::Exit) => return (s[8], mem.get(&0x30).copied().unwrap_or(0)),
                None => panic!("fell off the stub"),
            };
            assert!(decode(w, 4 * pc as u64).is_some(), "{w:#x} does not decode");
            let (rd, rn, rm) = (
                (w & 31) as usize,
                ((w >> 5) & 31) as usize,
                ((w >> 16) & 31) as usize,
            );
            let imm12 = u64::from((w >> 10) & 0xfff);
            let rel19 = ((w >> 5) & 0x7ffff) as i32;
            let rel19 = (rel19 << 13) >> 13;
            let rd_mem =
                |m: &std::collections::HashMap<u64, u32>, a: u64| m.get(&a).copied().unwrap_or(0);
            let mut next = pc + 1;
            if w & 0xffc0_0000 == 0xb940_0000 {
                x[rd] = u64::from(rd_mem(&mem, x[rn] + imm12 * 4)); // ldr w
            } else if w & 0xffc0_0000 == 0xf940_0000 {
                x[rd] = u64::from(rd_mem(&mem, x[rn] + imm12 * 8)); // ldr x (the high half is 0)
            } else if w & 0xffc0_0000 == 0xf900_0000 {
                mem.insert(x[rn] + imm12 * 8, x[rd] as u32); // str x
            } else if w & 0xffc0_0000 == 0xbd40_0000 {
                s[rd] = f32::from_bits(rd_mem(&mem, x[rn] + imm12 * 4)); // ldr s
            } else if w & 0xff00_0000 == 0x3400_0000 {
                if x[rd] as u32 == 0 {
                    next = (pc as i64 + i64::from(rel19)) as usize; // cbz w
                }
            } else if w & 0xff00_0010 == 0x5400_0000 {
                assert_eq!(w & 15, 1, "b.ne only");
                if !z {
                    next = (pc as i64 + i64::from(rel19)) as usize;
                }
            } else if w & 0xfc00_0000 == 0x1400_0000 {
                next = (pc as i64 + i64::from(((w << 6) as i32) >> 6)) as usize; // b
            } else if w & 0xffff_fc00 == 0x1e23_0000 {
                s[rd] = x[rn] as u32 as f32; // ucvtf s,w
            } else if w & 0xffff_fc1f == 0x1e20_2008 {
                z = s[rn] == 0.0; // fcmp s,#0.0
            } else if w & 0xffe0_fc00 == 0x1e20_2800 {
                s[rd] = s[rn] + s[rm]; // fadd
            } else if w & 0xffe0_fc00 == 0x1e20_0800 {
                s[rd] = s[rn] * s[rm]; // fmul
            } else if w & 0xffe0_fc00 == 0x1e20_1800 {
                s[rd] = s[rn] / s[rm]; // fdiv
            } else if w & 0xffe0_0000 == 0x52a0_0000 {
                x[rd] = u64::from((w >> 5) & 0xffff) << 16; // mov w,#imm,lsl 16
            } else if w & 0xffff_fc00 == 0x1e27_0000 {
                s[rd] = f32::from_bits(x[rn] as u32); // fmov s,w
            } else if w & 0xffe0_fc00 == 0x8b00_0000 {
                x[rd] = x[rn] + x[rm]; // add x,x,x
            } else {
                panic!("the model does not know {w:#x}");
            }
            pc = next;
        }
        panic!("the stub does not end")
    }

    fn page(target: f32, step: u32, var: f32) -> Vec<(u64, u32)> {
        vec![(0, target.to_bits()), (0x20, step), (0x50, var.to_bits())]
    }

    #[test]
    fn the_stub_gives_the_game_its_own_division_until_the_host_decides() {
        let own = BASE_RATE / GAME_RATE;
        // fps_target 0
        assert_eq!(run(&page(0.0, 1, 0.25)), (own, 0));
        // no step and no variable step yet, at 60 and at 120
        assert_eq!(run(&page(60.0, 0, 0.0)), (own, 0));
        assert_eq!(run(&page(120.0, 0, 0.0)), (own, 0));
    }

    #[test]
    fn a_whole_step_is_vsyncs_of_one_sixtieth() {
        // 60 fps: one vsync is the ratio base / 60, 30 fps: two
        assert_eq!(run(&page(60.0, 1, 0.0)).0, BASE_RATE / 60.0);
        assert_eq!(run(&page(30.0, 2, 0.0)).0, 2.0 * BASE_RATE / 60.0);
        // the vsyncs stepped are counted
        let mut p = page(60.0, 2, 0.0);
        p.push((0x30, 40));
        assert_eq!(run(&p), (2.0 * BASE_RATE / 60.0, 42));
    }

    #[test]
    fn at_120_the_variable_step_is_half_the_ratio_of_60_per_tick() {
        // the host's time step is 0.25 of 1/30 s at 120 fps: half a vsync, with the whole step 1 beside it
        let r120 = run(&page(120.0, 1, 0.25)).0;
        let r60 = run(&page(60.0, 1, 0.0)).0;
        assert_eq!(r120 * 2.0, r60);
        // two ticks of 120 Hz are the ratio of one at 60, and ratio times fps is the same at both
        assert_eq!((r120 * 120.0, r60 * 60.0), (BASE_RATE, BASE_RATE));
        // the floor, a slow tick and the ceiling of the host's step: its value in 1/30 s is twice its vsyncs
        for var in [0.125f32, 0.3333, 1.0, 1.5] {
            assert_eq!(run(&page(120.0, 1, var)).0, 2.0 * var * BASE_RATE / 60.0);
        }
        // the variable step wins over the whole step, and the stepped count is not kept
        assert_eq!(run(&page(120.0, 3, 0.25)), (r120, 0));
    }

    #[test]
    fn nxkit_runs_every_service() {
        assert!(
            Service::ALL
                .into_iter()
                .all(|s| OOT.runtime(s).map(|f| f.value) == Some(Runtime::Nxkit))
        );
    }
}
