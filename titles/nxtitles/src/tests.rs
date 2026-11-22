use super::*;

#[test]
fn the_catalog_builds_and_every_title_has_both_layers() {
    use nxtitle::{Layer, TitleFacts};
    let c = catalog().unwrap();
    // with every feature on, each title finds the engine family it names and the graphics family of its API
    if cfg!(feature = "default") {
        assert!(
            c.missing_families().is_empty(),
            "{}",
            c.missing_families().len()
        );
    }
    for t in c.titles() {
        let r = c.resolve(&TitleFacts::new(t.id()));
        assert_eq!(
            r.engine.map(|f| f.layer()).unwrap_or(Layer::Engine),
            Layer::Engine
        );
        assert_eq!(
            r.graphics.map(|f| f.layer()).unwrap_or(Layer::Graphics),
            Layer::Graphics
        );
        if let Some(g) = r.graphics {
            assert_eq!(g.id(), t.graphics().value.id(), "{}", t.name());
        }
    }
}

#[test]
fn every_family_is_named_by_a_title() {
    use nxtitle::Layer;
    let c = catalog().unwrap();
    for f in c.families() {
        let named = c.titles().iter().any(|t| match f.layer() {
            Layer::Engine => t.engine() == f.id(),
            Layer::Graphics => t.graphics().value.id() == f.id(),
        });
        assert!(named, "no title names the family {}", f.id());
    }
}

#[cfg(all(feature = "engine-unreal", feature = "octo"))]
#[test]
fn octopath_traveler_gets_the_unreal_frame_limit() {
    use nxtitle::{EngineFrom, LimitRule, LimitSite, TitleFacts, TitleId};
    let c = catalog().unwrap();
    let r = c.resolve(&TitleFacts::new(TitleId(0x0100_57D0_0649_2000)));
    assert_eq!(r.engine.map(|f| f.id()), Some("unreal"));
    assert_eq!(r.engine_from, EngineFrom::Declared);
    let sites: Vec<_> = r
        .limits()
        .iter()
        .filter_map(|l| match l.value {
            LimitRule::Row(row) => Some(row.site),
            LimitRule::Reflected(_) => None,
        })
        .collect();
    assert_eq!(
        sites,
        [LimitSite::Found {
            key: "UE_CVAR:t.MaxFPS",
            add: 0x10
        }]
    );
}

#[test]
fn every_seed_file_reaches_the_build_through_its_module() {
    use nxtitle::TitleFacts;
    let c = catalog().unwrap();
    let games = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../games");
    for t in c.titles() {
        let file = games.join(t.id().to_string()).join("pipelines.seeds");
        let hook = c.resolve(&TitleFacts::new(t.id())).pipeline_seeds();
        match std::fs::read_to_string(&file) {
            Ok(text) => {
                assert!(
                    text.starts_with("nxkit-pipeline-seeds 1\n"),
                    "{}",
                    file.display()
                );
                assert!(
                    hook.contains(&text.as_str()),
                    "{} has no pipeline_seeds hook",
                    t.name()
                );
            }
            Err(_) => assert!(hook.is_empty(), "{}", t.name()),
        }
    }
}

#[test]
fn every_title_builds_standalone() {
    use nxtitle::TitleFacts;
    let c = catalog().unwrap();
    for t in c.titles() {
        let r = c.resolve(&TitleFacts::new(t.id()));
        let st = r.standalone();
        assert!(st.is_some() && r.title_rules().is_some(), "{}", t.name());
        assert!(
            r.nxkit_services().contains(&nxtitle::Service::Kernel),
            "{}",
            t.name()
        );
        assert!(t.profile().is_some(), "{}", t.name());
    }
}

#[cfg(all(feature = "engine-agl", feature = "oot"))]
#[test]
fn oot_resolves_to_agl() {
    use nxtitle::{EngineFrom, TitleFacts, TitleId};
    let c = catalog().unwrap();
    let r = c.resolve(&TitleFacts::new(TitleId(0x0400_4860_24DE_C000)));
    assert_eq!(
        r.title.map(|t| t.name()),
        Some("The Legend of Zelda: Ocarina of Time")
    );
    assert_eq!(r.engine.map(|f| f.id()), Some("agl"));
    assert_eq!(r.engine_from, EngineFrom::Declared);
    assert_eq!(r.needs().len(), 6);
}

#[cfg(all(feature = "engine-agl", feature = "botw"))]
#[test]
fn botw_resolves_to_agl_without_the_oot_keys() {
    use nxtitle::{EngineFrom, TitleFacts, TitleId};
    let c = catalog().unwrap();
    let r = c.resolve(&TitleFacts::new(TitleId(0x0100_7EF0_0011_E000)));
    assert_eq!(r.engine.map(|f| f.id()), Some("agl"));
    assert_eq!(r.engine_from, EngineFrom::Declared);
    let keys: Vec<_> = r.needs().iter().map(|n| n.value.capability).collect();
    assert!(!keys.contains(&"render_cond") && !keys.contains(&"tex_flush_upload"));
    assert_eq!(keys.len(), 5);
    assert_eq!(r.nxkit_services(), nxtitle::Service::ALL);
    assert!(r.standalone().is_some());
}

#[cfg(all(feature = "engine-agl", feature = "smbw"))]
#[test]
fn smbw_resolves_to_agl_and_builds_standalone() {
    use nxtitle::{EngineFrom, TitleFacts, TitleId};
    let c = catalog().unwrap();
    let r = c.resolve(&TitleFacts::new(TitleId(0x0100_1510_0B51_4000)));
    assert_eq!(r.title.map(|t| t.name()), Some("Super Mario Bros. Wonder"));
    assert_eq!(r.engine.map(|f| f.id()), Some("agl"));
    assert_eq!(r.engine_from, EngineFrom::Declared);
    assert!(r.needs().is_empty());
    assert_eq!(r.nxkit_services(), nxtitle::Service::ALL);
    assert!(r.standalone().is_some() && r.title_rules().is_some());
}

#[test]
fn engine_markers_are_unique_and_each_family_finds_itself() {
    use nxtitle::{Layer, TitleFacts, TitleId};
    let c = catalog().unwrap();
    let mut seen = std::collections::BTreeMap::new();
    let mut libs = std::collections::BTreeMap::new();
    for f in c.families().iter().filter(|f| f.layer() == Layer::Engine) {
        for m in f.markers() {
            assert!(
                !m.pattern.is_empty(),
                "{}: a marker with no pattern",
                f.id()
            );
            assert!(m.pattern.len() <= 48, "{}: only short names", f.id());
            // a marker is claimed by one family, unless that family refines the other
            if let Some(other) = seen.insert(format!("{:?}:{}", m.at, m.pattern), f.id()) {
                assert_eq!(
                    f.refines(),
                    Some(other),
                    "{} and {other} share the marker {:?}",
                    f.id(),
                    m.pattern
                );
            }
            // a library id is named by one family only
            let owner = libs.insert(m.id, f.id());
            if let Some(o) = owner {
                assert_eq!(
                    o,
                    f.id(),
                    "the library {} is named by {o} and {}",
                    m.id,
                    f.id()
                );
            }
        }
        if f.markers().is_empty() {
            continue;
        }
        // facts that hold all its libraries are settled to this family, and to no other
        let mut facts = TitleFacts::new(TitleId(0));
        facts.libraries = f.markers().iter().map(|m| m.id.to_string()).collect();
        let d = c.detect_engine(&facts);
        assert_eq!(d.family, Some(f.id()), "{}: {:?}", f.id(), d.candidates);
    }
}

#[test]
fn a_game_with_no_marker_has_no_engine() {
    use nxtitle::{EngineFrom, TitleFacts, TitleId};
    let c = catalog().unwrap();
    // nobody looked, and a game with no trace of any engine
    for libs in [vec![], vec!["unlisted".to_string()]] {
        let mut facts = TitleFacts::new(TitleId(0x0100_7340_1175_E000));
        facts.libraries = libs;
        assert_eq!(c.resolve(&facts).engine_from, EngineFrom::None);
    }
}
