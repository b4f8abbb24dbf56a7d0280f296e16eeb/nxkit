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
}
impl RuntimeHooks for Fam {
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
    fn upscaler(&self) -> Option<Fact<Upscaler>> {
        Some(Fact::new(Upscaler::Native, REC))
    }
    fn runtime(&self, s: Service) -> Option<Fact<Runtime>> {
        match (self.0, s) {
            (1, Service::Fs) => Some(Fact::new(Runtime::Nxnative, REC)),
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
    assert_eq!(r.needs().len(), 1);
    let up = ShaderView {
        key: "0",
        stage: ShaderStage::Compute,
        archive: Some("up.bnsh"),
    };
    assert!(r.shader_role(&up).is_some());
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
    assert_eq!(r.runtime(Service::Fs), Runtime::Nxnative);
    assert_eq!(r.runtime(Service::Audio), Runtime::Nxkit);
    assert_eq!(r.runtime(Service::Gpu), Runtime::Nxnative);
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
