use super::*;

fn module(base: u64, size: u64) -> Module {
    Module {
        base,
        size,
        build_id: "00000000000000000000000000000000".to_string(),
        ..Module::default()
    }
}

fn config(module: Module, target: u32, fps_max: u32) -> LimitsConfig {
    LimitsConfig {
        module,
        target,
        fps_max,
        ue_fps_cvar: "t.MaxFPS".to_string(),
        dynamic_resolution: false,
    }
}

#[test]
fn a_module_without_a_limiter_has_none_and_writes_nothing() {
    let _g = crate::GUEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    guest::set_range(0x10000, 0);
    let (mut l, notices) = Limits::new(&config(module(0x10000, 0), 60, 60));
    assert!(!l.has_engine());
    assert!(notices.is_empty());
    assert_eq!(l.target(), 0);
    assert!(l.tick(1).is_ok_and(|e| e.is_empty()));
}

#[test]
fn an_unreal_main_without_the_frame_limit_variable_is_told() {
    let mut m = module(0x10000, 0);
    m.unreal = vec![UnrealCvar {
        name: "rhi.SyncInterval".to_string(),
        off: 0x100,
    }];
    let _g = crate::GUEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    guest::set_range(0x10000, 0);
    let (l, notices) = Limits::new(&config(m, 60, 60));
    assert!(l.has_engine(), "the present interval is a row");
    assert!(notices.iter().any(|n| n.red && n.text.contains("t.MaxFPS")));
    assert_eq!(l.target(), 60);
}

#[test]
fn a_target_of_120_needs_fps_max_and_runs_at_60_without() {
    let mut m = module(0x10000, 0);
    m.fps_target = Some(FpsTargetField {
        ptr_off: 0,
        field: 0x40,
    });
    let _g = crate::GUEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    guest::set_range(0x10000, 0);
    let (l, notices) = Limits::new(&config(m.clone(), 120, 60));
    assert_eq!((l.target(), l.runs_at_120()), (60, false));
    assert!(notices.iter().any(|n| n.red));
    let (l, notices) = Limits::new(&config(m, 120, 120));
    assert_eq!((l.target(), l.runs_at_120()), (120, true));
    assert!(notices.is_empty());
}

#[test]
fn a_target_of_zero_leaves_every_limiter_and_says_so() {
    let mut m = module(0x10000, 0);
    m.fps_target = Some(FpsTargetField {
        ptr_off: 0,
        field: 0x40,
    });
    let _g = crate::GUEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    guest::set_range(0x10000, 0);
    let (mut l, notices) = Limits::new(&config(m, 0, 60));
    assert!(l.has_engine());
    assert_eq!(l.target(), 0);
    assert!(
        notices
            .iter()
            .any(|n| n.text.contains("left as the game sets it"))
    );
    assert!(l.tick(1).is_ok_and(|e| e.is_empty()));
}

#[test]
fn the_target_goes_into_the_found_field_and_follows_the_hold_at_120() {
    let _g = crate::GUEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut mem = vec![0u64; 0x40];
    let base = mem.as_mut_ptr() as u64;
    guest::set_range(base, (mem.len() * 8) as u64);
    assert!(guest::write_u32(base + 0x40, 30).is_ok());
    let mut m = module(base, 0);
    m.fps_target = Some(FpsTargetField {
        ptr_off: 0,
        field: 0x40,
    });
    let (mut l, _) = Limits::new(&config(m.clone(), 60, 120));
    let events = l.tick(1);
    assert!(events.is_ok_and(|e| e.len() == 1));
    assert_eq!(guest::read_u32(base + 0x40).ok(), Some(60));
    assert!(guest::write_u32(base + 0x40, 30).is_ok());
    let (mut high, _) = Limits::new(&config(m, 120, 120));
    assert!(high.tick(1).is_ok());
    assert_eq!(guest::read_u32(base + 0x40).ok(), Some(120));
    assert!(guest::write_u32(base + 0x40, 30).is_ok());
    assert!(high.tick(2).is_ok());
    assert_eq!(
        guest::read_u32(base + 0x40).ok(),
        Some(60),
        "the hold is above one tick"
    );
}

#[test]
fn unreal_rows_write_the_game_thread_and_the_render_thread_copy() {
    let _g = crate::GUEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut mem = vec![0u64; 0x80];
    let base = mem.as_mut_ptr() as u64;
    guest::set_range(base, (mem.len() * 8) as u64);
    let var = base + 0x200;
    assert!(guest::write_u64(base + 0x110, var).is_ok());
    assert!(guest::write_u32(var, 30f32.to_bits()).is_ok());
    assert!(guest::write_u32(var + 4, 30f32.to_bits()).is_ok());
    let mut m = module(base, 0);
    m.unreal = vec![UnrealCvar {
        name: "t.MaxFPS".to_string(),
        off: 0x100,
    }];
    let (mut l, notices) = Limits::new(&config(m, 60, 60));
    assert!(notices.is_empty());
    assert!(l.tick(1).is_ok_and(|e| e.len() == 2));
    assert_eq!(guest::read_u32(var).ok(), Some(60f32.to_bits()));
    assert_eq!(guest::read_u32(var + 4).ok(), Some(60f32.to_bits()));
}
