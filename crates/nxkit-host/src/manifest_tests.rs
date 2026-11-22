use super::*;
use crate::module::samples::NEAR;

fn parse(extra: &str) -> Result<Game> {
    Game::parse(Path::new("/x"), &sample(extra, NEAR))
}

#[test]
fn a_build_without_a_title_module_gives_a_start_line() {
    assert_eq!(parse("").unwrap().generic, None);
    let g = parse(r#""generic": true, "engine": {"family": "unity", "markers": ["Unity"], "candidates": ["unity"]},"#)
        .unwrap();
    let line = g.generic.unwrap();
    assert!(
        line.contains("engine family unity") && line.contains("[Unity]"),
        "{line}"
    );
    let g = parse(
        r#""generic": true, "engine": {"family": null, "markers": [], "candidates": ["a", "b"]},"#,
    )
    .unwrap();
    assert!(
        g.generic
            .unwrap()
            .contains("no engine family (candidates: a, b)")
    );
}

#[test]
fn parses_a_sample() {
    let r = parse("");
    assert!(r.is_ok(), "{:?}", r.as_ref().err());
    let Ok(g) = r else { return };
    assert_eq!(g.modules[0].base, 0x1_0000_0000);
    assert_eq!(g.modules[0].end(), 0x1_0002_0000);
    assert_eq!(g.main_thread.stack_size, 0x10_0000);
    assert_eq!((g.version.as_str(), g.version_number), ("1.6.0", 1_114_112));
    assert_eq!(g.settings.fps_target, 30);
    assert_eq!(g.main_module().map(|m| m.name.as_str()).ok(), Some("main"));
    assert_eq!(g.graphics, Graphics::Nvn);
}

#[test]
fn the_graphics_api_is_read() {
    let g = parse(r#""graphics": "vulkan","#).map(|g| g.graphics).ok();
    assert_eq!(g, Some(Graphics::Vulkan));
    assert!(parse(r#""graphics": "gl","#).is_err());
}

#[test]
fn the_heap_follows_from_reserve() {
    let r = parse("");
    assert!(r.is_ok(), "{:?}", r.as_ref().err());
    let Ok(g) = r else { return };
    assert_eq!(g.memory.heap, 0xd000_0000);
    assert_eq!(g.memory.app_pool, 0xcd50_0000);
    assert!(g.memory.map_limit && g.memory.alloc_big_align);
    assert_eq!(g.memory.stdalloc_cap, 5);
    assert_eq!(g.memory.mem_used, 0x400_0000);
}

#[test]
fn a_smaller_split_shrinks_the_heap_or_the_alias_region() {
    let r = parse("");
    assert!(r.is_ok(), "{:?}", r.as_ref().err());
    let Ok(g) = r else { return };
    assert_eq!(g.memory.reserve(), 0x1_0000_0000);
    let m = g.memory.resized(Some(0xc000_0000), None);
    assert_eq!(
        m.as_ref().map(|m| (m.heap, m.alias)).ok(),
        Some((0x9000_0000, 0x1000_0000))
    );
    let m = g.memory.resized(None, Some(0x800_0000));
    assert_eq!(
        m.as_ref().map(|m| (m.heap, m.alias)).ok(),
        Some((0xd800_0000, 0x800_0000))
    );
    assert!(g.memory.resized(Some(0x1000_0000), None).is_err());
}

#[test]
fn refuses_a_reserve_that_is_too_small() {
    let s = sample("", NEAR).replace("\"reserve\": 4294967296", "\"reserve\": \"0x1000\"");
    assert!(Game::parse(Path::new("/x"), &s).is_err());
}

#[test]
fn refuses_a_missing_memory_key() {
    let s = sample("", NEAR).replace("\"app_pool\": 3444572160,", "");
    assert!(Game::parse(Path::new("/x"), &s).is_err());
}

#[test]
fn refuses_another_page_size() {
    let s = sample("", NEAR).replace("\"page_size\": 16384", "\"page_size\": 4096");
    assert!(Game::parse(Path::new("/x"), &s).is_err());
}

#[test]
fn refuses_a_missing_segment_list() {
    let s = sample("", &NEAR.replace("\"segments\"", "\"parts\""));
    assert!(Game::parse(Path::new("/x"), &s).is_err());
}

#[test]
fn moves_every_module() {
    let g = parse("").and_then(|g| g.moved(0x8000));
    assert!(g.is_ok());
    let Ok(g) = g else { return };
    assert_eq!(g.modules[0].base, 0x1_0000_8000);
}

#[test]
fn reads_the_aoc_folders() {
    let r = parse(
        "\"aoc\": [{\"index\": 1, \"title_id\": \"01007EF00011F001\", \"version\": 196608, \"path\": \"aoc/1\"}],",
    );
    assert!(r.is_ok(), "{:?}", r.as_ref().err());
    let Ok(g) = r else { return };
    assert_eq!(g.aoc_dirs(), vec![(1, PathBuf::from("/x/aoc/1"))]);
}

#[test]
fn reads_a_list() {
    let l = parse_slots(r#"[{"name": "_ZN2nn2os5SleepE", "slot": "0x40"}]"#).unwrap_or_default();
    assert_eq!(
        l,
        vec![Slot {
            name: "_ZN2nn2os5SleepE".into(),
            slot: 0x40
        }]
    );
}

#[test]
fn reads_an_import_list_with_addend_and_weak() {
    let l = parse_imports(r#"[{"name": "f", "slot": "0x40", "addend": -8, "weak": true}]"#)
        .unwrap_or_default();
    assert_eq!(
        l,
        vec![Import {
            name: "f".into(),
            slot: 0x40,
            addend: -8,
            weak: true
        }]
    );
    assert!(parse_imports(r#"[{"name": "f", "slot": 64}]"#).is_err());
}

#[test]
fn a_render_size_fills_the_arguments_of_the_game() {
    let mut g = parse(r#""render_args": {"size": "-ResX={w} -ResY={h}"},"#).unwrap();
    assert_eq!(g.program_args(None), "");
    assert_eq!(g.program_args(Some((1920, 1080))), "-ResX=1920 -ResY=1080");
    g.settings.args = "-x".into();
    assert_eq!(
        g.program_args(Some((3840, 2160))),
        "-x -ResX=3840 -ResY=2160"
    );
    g.render_size_args = None;
    assert_eq!(
        g.program_args(Some((1920, 1080))),
        "-x",
        "no arguments: the game's own size"
    );
}
