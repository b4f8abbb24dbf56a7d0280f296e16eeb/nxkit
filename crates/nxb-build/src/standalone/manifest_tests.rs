use super::*;

fn manifest(generic: Option<Generic>) -> String {
    game_json(&Manifest {
        title_id: 1,
        name: "g",
        version: 0,
        version_name: "0".into(),
        modules: &[],
        services: vec![],
        graphics: "nvn",
        nvn_functions: vec![],
        patches: false,
        main_thread: MainThread {
            priority: 44,
            core: 0,
            stack_size: 0x100000,
        },
        aoc: &[],
        settings: nxtitle::Settings::DEFAULT,
        shared_font_decode: false,
        sf_services: vec![],
        limits: vec![],
        render_args: None,
        temporal_upscale: None,
        vertex_stream_null_bind: false,
        generic,
    })
}

#[test]
fn a_build_without_a_title_module_says_so_with_its_evidence() {
    let t = manifest(Some(Generic {
        engine: Some("unity"),
        hits: vec!["Unity"],
        candidates: vec!["unity"],
    }));
    assert!(t.contains("\"generic\": true"), "{t}");
    assert!(t.contains("\"family\": \"unity\""), "{t}");
    assert!(t.contains("\"Unity\""), "{t}");
    let none = manifest(Some(Generic {
        engine: None,
        hits: vec![],
        candidates: vec![],
    }));
    assert!(none.contains("\"family\": null"), "{none}");
    // a title module's build writes neither key
    let title = manifest(None);
    assert!(
        !title.contains("\"generic\"") && !title.contains("\"engine\""),
        "{title}"
    );
}

#[test]
fn the_nvn_list_skips_blank_and_comment_lines() {
    assert_eq!(nvn_list("# x\nnvnA\n\n nvnB \n"), ["nvnA", "nvnB"]);
}

#[test]
fn the_memory_object_carries_the_layout() {
    let mut t = nxtitle::Settings::DEFAULT;
    t.memory.app_pool = 0xCD50_0000;
    let v = memory(&t.memory);
    let text = nxb_pyjson::dumps(&v);
    assert!(text.contains(r#""app_pool": 3444572160"#), "{text}");
    assert!(text.contains(r#""map_limit": false"#));
    assert!(
        text.contains(r#""mem_used": "#) && text.contains(r#""sys_resource": "#),
        "{text}"
    );
}

#[test]
fn symbols_are_a_list_of_name_and_slot() {
    let t = symbols_json(&[Sym {
        name: "nnFoo".into(),
        slot: 768,
    }]);
    let v = nxb_pyjson::parse(&t).unwrap();
    assert_eq!(nxb_pyjson::dumps(&v), r#"[{"name": "nnFoo", "slot": 768}]"#);
}

#[test]
fn imports_carry_the_addend_and_the_weak_flag() {
    let t = imports_json(&[Import {
        name: "nnFoo".into(),
        slot: 768,
        addend: 8,
        weak: true,
    }]);
    let v = nxb_pyjson::parse(&t).unwrap();
    assert_eq!(
        nxb_pyjson::dumps(&v),
        r#"[{"name": "nnFoo", "slot": 768, "addend": 8, "weak": true}]"#
    );
}

#[test]
fn a_limit_row_names_its_site_and_kind() {
    use nxtitle::{LimitKind, LimitRow, LimitRule, LimitSite};
    let found = limit(&LimitRule::Row(LimitRow {
        what: "Unreal t.MaxFPS",
        build: None,
        site: LimitSite::Found {
            key: "t.MaxFPS",
            add: 0x10,
        },
        kind: LimitKind::FpsF32,
        copies: 2,
    }));
    assert_eq!(
        nxb_pyjson::dumps(&found),
        r#"{"what": "Unreal t.MaxFPS", "site": {"found": "t.MaxFPS", "add": 16}, "kind": {"id": "fps_f32"}, "copies": 2}"#
    );
    let fixed = limit(&LimitRule::Row(LimitRow {
        what: "w",
        build: Some("ab"),
        site: LimitSite::Fixed {
            ptr_off: 8,
            field: 4,
        },
        kind: LimitKind::TicksU32 { scale: 19_200_000 },
        copies: 1,
    }));
    assert_eq!(
        nxb_pyjson::dumps(&fixed),
        r#"{"what": "w", "build": "ab", "site": {"ptr_off": 8, "field": 4}, "kind": {"id": "ticks_u32", "scale": 19200000}, "copies": 1}"#
    );
}

/// The temporal upscale object of OCTOPATH TRAVELER, read back with the host's expectations (`tupscale.rs`).
#[test]
fn temporal_upscale_has_the_shape_the_host_reads() {
    let c = nxtitles::catalog().unwrap();
    let r = c.resolve(&nxtitle::TitleFacts::new(nxtitle::TitleId(
        0x0100_57D0_0649_2000,
    )));
    let (view, draws) = (
        r.temporal_view().unwrap().value,
        r.temporal_draws().unwrap().value,
    );
    let text = nxb_pyjson::dumps_indent(&temporal_upscale(&view, &draws), 1);
    println!("{text}");
    let v = nxb_pyjson::parse(&text).unwrap();
    let int = |o: &Value, k: &str| o.get(k).and_then(Value::as_i64).unwrap();
    let pair = |o: &Value, k: &str| -> Vec<f64> {
        o.get(k)
            .and_then(Value::as_list)
            .unwrap()
            .iter()
            .map(|x| match x {
                Value::Float(f) => *f,
                other => panic!("{other:?}"),
            })
            .collect()
    };
    let up = v.get("upscale").unwrap();
    assert_eq!(
        up.get("fragment").and_then(Value::as_str),
        Some("be89b1ada1308881")
    );
    assert_eq!(int(up, "color_slot"), 0);
    let src = v.get("source").unwrap();
    assert_eq!((int(src, "depth_slot"), int(src, "velocity_slot")), (3, 0));
    let ubo = src.get("view").unwrap();
    assert_eq!(ubo.get("stage").and_then(Value::as_str), Some("fragment"));
    assert_eq!(int(ubo, "slot"), 0);
    let pass = v.get("passthrough").and_then(Value::as_list).unwrap();
    assert_eq!(pass.len(), 2);
    assert_eq!(int(&pass[1], "slot"), 2);
    let offsets = v.get("view").unwrap();
    assert_eq!(
        [
            int(offsets, "clip_to_prev_clip"),
            int(offsets, "jitter"),
            int(offsets, "view_size")
        ],
        [1760, 1824, 1888]
    );
    let vel = v.get("velocity").unwrap();
    assert_eq!(pair(vel, "scale"), [4.008016, 4.008016]);
    assert_eq!(pair(vel, "bias"), [-2.003976, -2.003976]);
    assert_eq!(vel.get("y_up"), Some(&Value::Bool(true)));
    assert_eq!(vel.get("zero_means_camera"), Some(&Value::Bool(true)));
    assert_eq!(v.get("depth_reversed"), Some(&Value::Bool(true)));
}
