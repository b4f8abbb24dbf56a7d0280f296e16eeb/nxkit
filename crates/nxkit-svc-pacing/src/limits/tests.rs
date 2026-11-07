use super::*;

const BUILD: &str = "00000000000000000000000000000000";
const OTHER: &str = "ffffffffffffffffffffffffffffffff";

fn module(base: u64, size: u64) -> Module {
    Module {
        base,
        size,
        build_id: BUILD.to_string(),
        ..Module::default()
    }
}

fn config(module: Module, target: u32, rows: Vec<RowSpec>) -> LimitsConfig {
    LimitsConfig {
        module,
        target,
        rows,
        reflected: None,
    }
}

fn fixed(build: &'static str, field: u64, kind: Kind) -> RowSpec {
    RowSpec {
        what: "a value",
        build: Some(build),
        site: Site::Fixed { ptr_off: 0, field },
        kind,
        copies: 1,
    }
}

fn found(key: &'static str, kind: Kind, copies: u32) -> RowSpec {
    RowSpec {
        what: "a variable",
        build: None,
        site: Site::Found { key, add: 0x10 },
        kind,
        copies,
    }
}

#[test]
fn a_module_without_a_row_has_no_limit_and_writes_nothing() {
    let _g = crate::GUEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    guest::set_range(0x10000, 0);
    let (mut l, notices) = Limits::new(&config(module(0x10000, 0), 60, Vec::new()));
    assert!(!l.has_engine());
    assert!(notices.is_empty());
    assert!(l.tick().is_ok_and(|e| e.is_empty()));
}

#[test]
fn a_build_without_an_entry_is_told() {
    let _g = crate::GUEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    guest::set_range(0x10000, 0);
    let row = fixed(OTHER, 0x40, Kind::FpsI32);
    let (l, notices) = Limits::new(&config(module(0x10000, 0), 60, vec![row]));
    assert!(!l.has_engine());
    assert!(notices.iter().any(|n| n.red && n.text.contains(BUILD)));
}

#[test]
fn a_name_that_the_rewriter_did_not_find_is_told() {
    let _g = crate::GUEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    guest::set_range(0x10000, 0);
    let (l, notices) = Limits::new(&config(
        module(0x10000, 0),
        60,
        vec![found("console.var", Kind::FpsF32, 2)],
    ));
    assert!(!l.has_engine());
    assert!(
        notices
            .iter()
            .any(|n| n.red && n.text.contains("console.var"))
    );
}

#[test]
fn a_target_of_120_is_refused_for_an_engine_limit() {
    let _g = crate::GUEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    guest::set_range(0x10000, 0);
    let row = fixed(BUILD, 0x40, Kind::FpsI32);
    let (l, notices) = Limits::new(&config(module(0x10000, 0), 120, vec![row]));
    assert!(l.has_engine());
    assert_eq!(l.target(), 0);
    assert!(notices.iter().any(|n| n.red));
}

#[test]
fn a_target_of_zero_leaves_the_limits_and_still_writes_a_pinned_word() {
    let _g = crate::GUEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut mem = vec![0u64; 0x40];
    let base = mem.as_mut_ptr() as u64;
    guest::set_range(base, (mem.len() * 8) as u64);
    assert!(guest::write_u32(base + 0x40, 30).is_ok());
    assert!(guest::write_u32(base + 0x80, 0x3f33_3333).is_ok());
    let rows = vec![
        fixed(BUILD, 0x40, Kind::FpsI32),
        fixed(
            BUILD,
            0x80,
            Kind::PinU32 {
                from: 0x3f33_3333,
                to: 0x3f80_0000,
            },
        ),
    ];
    let (mut l, notices) = Limits::new(&config(module(base, 0), 0, rows));
    assert!(l.has_engine());
    assert_eq!(l.target(), 0);
    assert!(notices.iter().any(|n| n.text.contains("stay as the game")));
    assert!(l.tick().is_ok_and(|e| e.len() == 1));
    assert_eq!(guest::read_u32(base + 0x40).ok(), Some(30), "left alone");
    assert_eq!(guest::read_u32(base + 0x80).ok(), Some(0x3f80_0000));
}

#[test]
fn the_target_goes_into_the_row_of_the_build() {
    let _g = crate::GUEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut mem = vec![0u64; 0x40];
    let base = mem.as_mut_ptr() as u64;
    guest::set_range(base, (mem.len() * 8) as u64);
    assert!(guest::write_u32(base + 0x40, 30).is_ok());
    let rows = vec![
        fixed(BUILD, 0x40, Kind::FpsI32),
        fixed(OTHER, 0x44, Kind::FpsI32),
    ];
    let (mut l, _) = Limits::new(&config(module(base, 0), 60, rows));
    assert_eq!(l.target(), 60);
    assert!(l.tick().is_ok_and(|e| e.len() == 1));
    assert_eq!(guest::read_u32(base + 0x40).ok(), Some(60));
    assert!(
        l.tick().is_ok_and(|e| e.is_empty()),
        "nothing left to change"
    );
}

#[test]
fn a_found_row_writes_the_game_thread_and_the_render_thread_copy() {
    let _g = crate::GUEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut mem = vec![0u64; 0x80];
    let base = mem.as_mut_ptr() as u64;
    guest::set_range(base, (mem.len() * 8) as u64);
    let var = base + 0x200;
    assert!(guest::write_u64(base + 0x110, var).is_ok());
    assert!(guest::write_u32(var, 30f32.to_bits()).is_ok());
    assert!(guest::write_u32(var + 4, 30f32.to_bits()).is_ok());
    let mut m = module(base, 0);
    m.found = vec![Found {
        key: "console.var".to_string(),
        off: 0x100,
    }];
    let (mut l, notices) = Limits::new(&config(m, 60, vec![found("console.var", Kind::FpsF32, 2)]));
    assert!(notices.is_empty());
    assert!(l.tick().is_ok_and(|e| e.len() == 2));
    assert_eq!(guest::read_u32(var).ok(), Some(60f32.to_bits()));
    assert_eq!(guest::read_u32(var + 4).ok(), Some(60f32.to_bits()));
}
