//! The arena the host keeps at the end of the alias region, and the SDK's address regions in front of it.
use super::*;

const ARENA: u64 = 0x4_0000;

#[allow(clippy::expect_used, reason = "a test helper")]
fn arena_rig() -> Rig {
    let mem = Arc::new(FakeMemory::new(BASE, (SIZE / 4) as usize));
    let config = Config {
        memory: Some(MemoryConfig {
            host: mem.clone(),
            base: BASE,
            size: SIZE,
            front_size: 0x10_0000,
            stack_size: 0x10_0000,
            alias_size: 0x10_0000,
            arena: ARENA,
            app_pool: 0x40_0000,
            used_memory: 0x10_0000,
            system_resource: 0x20_0000,
            map_limit: false,
            program_id: 1,
            tls: (BASE, 0x1_0000),
            shared: (BASE + 0x2_0000, 0x1_0000),
            main_stack: None,
            modules: Vec::new(),
        }),
        main_thread: Some(MainThread {
            priority: 28,
            core: 1,
            stack_size: 0x10_0000,
        }),
        log: |_| {},
        ..Config::default()
    };
    let kernel = Kernel::try_new(config, mem.clone()).expect("the layout fits");
    let main = kernel.attach_main_thread("main").expect("attach");
    Rig { kernel, mem, main }
}

#[test]
fn the_alias_region_leaves_the_arena_out_and_a_stack_can_come_from_it() {
    let r = arena_rig();
    let end = BASE + SIZE;
    assert_eq!(r.kernel.arena(), Some((end - ARENA, ARENA)));
    let info = |id: u64| r.svc(0x29, &[0, id, 0xFFFF_8001, 0]);
    assert_eq!(info(2)[1], ALIAS, "alias region address");
    assert_eq!(
        info(3)[1],
        0x10_0000 - ARENA,
        "the alias region ends at the arena"
    );
    // MapPhysicalMemory does not reach into the arena
    assert_ne!(r.svc(0x2C, &[end - ARENA, 0x4000])[0], 0);
    assert_eq!(r.svc(0x2C, &[ALIAS, 0x4000])[0], 0);
    // the arena is normal memory: a thread stack from it maps into the stack region
    assert_eq!(r.svc(0x04, &[STACK, end - ARENA + 0x1_0000, 0x4000])[0], 0);
    assert_eq!(r.svc(0x05, &[STACK, end - ARENA + 0x1_0000, 0x4000])[0], 0);
}

#[test]
fn an_alias_region_too_small_for_more_regions_has_no_room() {
    let r = arena_rig();
    let k = &r.kernel;
    // the alias region of the rig is smaller than the node area and the room for later regions
    assert_eq!(k.alias_room(), 0);
    k.region_ask(0x1000);
    k.region_got(ALIAS);
    k.region_free(ALIAS);
    assert_eq!(k.alias_room(), 0);
}
