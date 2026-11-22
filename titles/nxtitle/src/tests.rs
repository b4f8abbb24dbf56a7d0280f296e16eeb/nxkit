use crate::*;
use nxb_model::TitleId;

const REC: Evidence = Evidence::Unverified { note: "test" };

struct Fam;
impl BuildHooks for Fam {
    fn shader_role(&self, s: &ShaderView<'_>) -> Option<Fact<ShaderRole>> {
        (s.archive == Some("up.bnsh"))
            .then_some(Fact::new(ShaderRole::Upscaler { name: "fam" }, REC))
    }
    fn seeds(&self) -> &'static [Seed] {
        &[Seed {
            key: "f",
            seen: "boot",
        }]
    }
    fn rewrite_rules(&self) -> Option<&'static str> {
        Some("family")
    }
    fn console_vars(&self) -> &'static [Fact<ConsoleVar>] {
        const V: &[Fact<ConsoleVar>] = &[
            Fact::new(
                ConsoleVar {
                    name: "a",
                    value: "1",
                },
                REC,
            ),
            Fact::new(
                ConsoleVar {
                    name: "b",
                    value: "2",
                },
                REC,
            ),
        ];
        V
    }
    fn config_keys(&self) -> &'static [Fact<ConfigKey>] {
        const K: &[Fact<ConfigKey>] = &[Fact::new(
            ConfigKey {
                section: "S",
                key: "k",
                value: "1",
            },
            REC,
        )];
        K
    }
    fn console_var_files(
        &self,
        set: ConsoleSettings<'_>,
        _romfs: &dyn RomfsRead,
    ) -> Option<Result<Vec<(String, Vec<u8>)>, String>> {
        Some(Ok(vec![(
            "f".into(),
            format!("{} {}", set.vars.len(), set.keys.len()).into_bytes(),
        )]))
    }
}
const VELOCITY: VelocityCode = VelocityCode {
    scale: [2.0, 2.0],
    bias: [-1.0, -1.0],
    y_up: true,
    zero_means_camera: false,
};

fn view(jitter: u32) -> Fact<TemporalView> {
    Fact::new(
        TemporalView {
            clip_to_prev_clip: 0,
            jitter,
            view_size: 8,
            velocity: VELOCITY,
            depth_reversed: false,
        },
        REC,
    )
}

impl RuntimeHooks for Fam {
    fn temporal_view(&self) -> Option<Fact<TemporalView>> {
        Some(view(4))
    }
    fn needs(&self) -> &'static [Fact<Need>] {
        const N: &[Fact<Need>] = &[Fact::new(
            Need {
                capability: "c",
                setting: "on",
                why: "w",
            },
            REC,
        )];
        N
    }
    fn pacing(&self) -> Option<Fact<Pacing>> {
        Some(Fact::new(Pacing::Display, REC))
    }
    fn limits(&self) -> &'static [Fact<LimitRule>] {
        const L: &[Fact<LimitRule>] = &[Fact::new(
            LimitRule::Row(LimitRow {
                what: "family",
                build: None,
                site: LimitSite::Found { key: "k", add: 0 },
                kind: LimitKind::FpsF32,
                copies: 1,
            }),
            REC,
        )];
        L
    }
    fn pace_profile(&self) -> Option<Fact<PaceProfile>> {
        Some(Fact::new(
            PaceProfile {
                fps_target: 30,
                fps_hold: false,
                present_snap: false,
                present_interval: PresentInterval::Display,
            },
            REC,
        ))
    }
    fn runtime(&self, s: Service) -> Option<Fact<Runtime>> {
        matches!(s, Service::Audio | Service::Fs).then_some(Fact::new(Runtime::Nxkit, REC))
    }
    fn upscaler(&self) -> Option<Fact<Upscaler>> {
        Some(Fact::new(Upscaler::MetalFx, REC))
    }
}
impl Family for Fam {
    fn id(&self) -> &'static str {
        "fam"
    }
    fn name(&self) -> &'static str {
        "Family"
    }
    fn detect(&self, f: &TitleFacts) -> Match {
        if f.has_library("famlib") {
            Match::Yes
        } else {
            Match::Unknown
        }
    }
}

struct Game(u64, Option<&'static str>);
impl BuildHooks for Game {
    fn console_vars(&self) -> &'static [Fact<ConsoleVar>] {
        const V: &[Fact<ConsoleVar>] = &[
            Fact::new(
                ConsoleVar {
                    name: "a",
                    value: "9",
                },
                REC,
            ),
            Fact::new(
                ConsoleVar {
                    name: "c",
                    value: "3",
                },
                REC,
            ),
        ];
        V
    }
    fn config_keys(&self) -> &'static [Fact<ConfigKey>] {
        const K: &[Fact<ConfigKey>] = &[
            Fact::new(
                ConfigKey {
                    section: "S",
                    key: "k",
                    value: "2",
                },
                REC,
            ),
            Fact::new(
                ConfigKey {
                    section: "T",
                    key: "k",
                    value: "3",
                },
                REC,
            ),
        ];
        K
    }
    fn graphics_api(&self) -> Option<Fact<GraphicsApi>> {
        (self.0 == 2).then_some(Fact::new(GraphicsApi::Vulkan, REC))
    }
    fn seeds(&self) -> &'static [Seed] {
        &[
            Seed {
                key: "t1",
                seen: "title",
            },
            Seed {
                key: "t2",
                seen: "title",
            },
        ]
    }
}
impl RuntimeHooks for Game {
    fn limits(&self) -> &'static [Fact<LimitRule>] {
        const L: &[Fact<LimitRule>] = &[Fact::new(
            LimitRule::Row(LimitRow {
                what: "title",
                build: Some("00"),
                site: LimitSite::Fixed {
                    ptr_off: 0,
                    field: 8,
                },
                kind: LimitKind::FpsI32,
                copies: 1,
            }),
            REC,
        )];
        L
    }
    fn upscaler(&self) -> Option<Fact<Upscaler>> {
        Some(Fact::new(Upscaler::Native, REC))
    }
    fn temporal_view(&self) -> Option<Fact<TemporalView>> {
        (self.0 == 2).then(|| view(12))
    }
    fn temporal_draws(&self) -> Option<Fact<TemporalDraws>> {
        let d = DrawSlot {
            fragment: "0123456789abcdef",
            slot: 1,
        };
        Some(Fact::new(
            TemporalDraws {
                upscale: d,
                source: SourceDraw {
                    fragment: d.fragment,
                    depth_slot: 0,
                    velocity_slot: 1,
                    view_stage: UboStage::Vertex,
                    view_slot: 2,
                },
                passthrough: &[],
            },
            REC,
        ))
    }
    fn runtime(&self, s: Service) -> Option<Fact<Runtime>> {
        match (self.0, s) {
            (1, Service::Fs) => Some(Fact::new(Runtime::Engine, REC)),
            (2, Service::Pacing) => Some(Fact::new(Runtime::Nxkit, REC)),
            _ => None,
        }
    }
}
impl Title for Game {
    fn id(&self) -> TitleId {
        TitleId(self.0)
    }
    fn name(&self) -> &'static str {
        "Game"
    }
    fn family(&self) -> Option<&'static str> {
        self.1
    }
}

static FAM: Fam = Fam;
static GAME: Game = Game(1, Some("fam"));
static LONE: Game = Game(2, None);
static ORPHAN: Game = Game(3, Some("gone"));

fn catalog() -> Catalog {
    Catalog::new(vec![&FAM], vec![&GAME, &LONE, &ORPHAN]).unwrap()
}

#[test]
fn title_wins_then_family_then_core() {
    let c = catalog();
    let r = c.resolve(&TitleFacts::new(TitleId(1)));
    assert_eq!(r.family_from, FamilyFrom::Declared);
    assert_eq!(r.upscaler().map(|f| f.value), Some(Upscaler::Native));
    assert_eq!(r.pacing().map(|f| f.value), Some(Pacing::Display));
    assert_eq!(r.pace_profile().map(|f| f.value.fps_target), Some(30));
    let keys: Vec<_> = r.seeds().iter().map(|s| s.key).collect();
    assert_eq!(keys, ["f", "t1", "t2"]);
    assert_eq!(r.rewrite_rules(), ["family"]);
    let whats: Vec<_> = r
        .limits()
        .iter()
        .filter_map(|f| match f.value {
            LimitRule::Row(row) => Some(row.what),
            LimitRule::Reflected(_) => None,
        })
        .collect();
    assert_eq!(whats, ["family", "title"]);
    assert_eq!(r.needs().len(), 1);
    let up = ShaderView {
        key: "0",
        stage: ShaderStage::Compute,
        archive: Some("up.bnsh"),
    };
    assert!(r.shader_role(&up).is_some());
}

#[test]
fn the_temporal_view_comes_from_the_first_layer_that_answers() {
    let c = catalog();
    let jitter = |id| {
        c.resolve(&TitleFacts::new(TitleId(id)))
            .temporal_view()
            .map(|f| f.value.jitter)
    };
    assert_eq!(jitter(1), Some(4));
    assert_eq!(jitter(2), Some(12));
    assert_eq!(UboStage::Vertex.id(), "vertex");
}

#[test]
fn the_graphics_api_is_the_titles_or_nvn() {
    let c = catalog();
    assert_eq!(
        c.resolve(&TitleFacts::new(TitleId(1))).graphics_api(),
        GraphicsApi::Nvn
    );
    assert_eq!(
        c.resolve(&TitleFacts::new(TitleId(2))).graphics_api(),
        GraphicsApi::Vulkan
    );
    assert_eq!(GraphicsApi::Vulkan.id(), "vulkan");
}

#[test]
fn family_by_detection_or_none() {
    let c = catalog();
    let mut facts = TitleFacts::new(TitleId(2));
    assert_eq!(c.resolve(&facts).family_from, FamilyFrom::None);
    assert!(c.resolve(&facts).pacing().is_none());
    assert!(c.resolve(&facts).pace_profile().is_none());
    facts.libraries.push("famlib".into());
    let r = c.resolve(&facts);
    assert_eq!(r.family_from, FamilyFrom::Detected);
    assert_eq!(r.upscaler().map(|f| f.value), Some(Upscaler::Native));
    let unknown = c.resolve(&TitleFacts::new(TitleId(9)));
    assert!(unknown.title.is_none() && unknown.family.is_none());
}

#[test]
fn a_missing_family_falls_back_to_the_core() {
    let c = catalog();
    let r = c.resolve(&TitleFacts::new(TitleId(3)));
    assert_eq!(r.family_from, FamilyFrom::None);
    assert_eq!(r.pacing(), None);
    let missing: Vec<_> = c
        .missing_families()
        .iter()
        .map(|(t, f)| (t.id(), *f))
        .collect();
    assert_eq!(missing, [(TitleId(3), "gone")]);
}

#[test]
fn duplicates_fail() {
    assert!(Catalog::new(vec![&FAM, &FAM], vec![]).is_err());
    assert!(Catalog::new(vec![], vec![&GAME, &GAME]).is_err());
}

#[test]
fn evidence_checked() {
    assert!(Evidence::Tested { test: "t" }.is_checked());
    assert!(
        Evidence::Recorded {
            source: "s",
            read: "2026-11-07"
        }
        .is_checked()
    );
    assert!(!REC.is_checked());
}

#[test]
fn each_service_runs_where_the_title_then_the_family_says() {
    let c = catalog();
    let r = c.resolve(&TitleFacts::new(TitleId(1)));
    assert_eq!(r.runtime(Service::Fs), Runtime::Engine);
    assert_eq!(r.runtime(Service::Audio), Runtime::Nxkit);
    assert_eq!(r.runtime(Service::Gpu), Runtime::Engine);
    assert_eq!(r.nxkit_services(), [Service::Audio]);
    let lone = c.resolve(&TitleFacts::new(TitleId(2)));
    assert_eq!(lone.nxkit_services(), [Service::Pacing]);
    let none = c.resolve(&TitleFacts::new(TitleId(0xdead)));
    assert!(none.nxkit_services().is_empty());
}

#[test]
fn service_ids_are_unique() {
    let ids: std::collections::BTreeSet<_> = Service::ALL.iter().map(|s| s.id()).collect();
    assert_eq!(ids.len(), Service::ALL.len());
}

struct NoRomfs;
impl RomfsRead for NoRomfs {
    fn list(&self, _: &str) -> Result<Option<Vec<RomfsEntry>>, String> {
        Ok(None)
    }
    fn len(&self, _: &str) -> Result<Option<u64>, String> {
        Ok(None)
    }
    fn read_range(&self, _: &str, _: u64, _: usize) -> Result<Vec<u8>, String> {
        Err("none".into())
    }
}

#[test]
fn console_vars_add_up_and_a_later_value_wins() {
    let c = catalog();
    let r = c.resolve(&TitleFacts::new(TitleId(1)));
    let vars: Vec<_> = r
        .console_vars()
        .iter()
        .map(|f| (f.value.name, f.value.value))
        .collect();
    assert_eq!(vars, [("a", "9"), ("b", "2"), ("c", "3")]);
    let keys: Vec<_> = r
        .config_keys()
        .iter()
        .map(|f| (f.value.section, f.value.key, f.value.value))
        .collect();
    assert_eq!(
        keys,
        [("S", "k", "2"), ("T", "k", "3")],
        "the title's value wins"
    );
    let files = r
        .console_var_files(ConsoleSettings::default(), &NoRomfs)
        .unwrap()
        .unwrap();
    assert_eq!(files, [("f".to_string(), b"0 0".to_vec())]);
    let lone = c.resolve(&TitleFacts::new(TitleId(2)));
    assert!(
        lone.console_var_files(ConsoleSettings::default(), &NoRomfs)
            .is_none()
    );
}
