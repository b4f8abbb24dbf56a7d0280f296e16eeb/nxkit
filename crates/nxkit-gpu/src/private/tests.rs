//! Tests of the private GPU memory: tracking, and the guest and frame helpers of the other tests.

use super::*;
use crate::detail::{DispatchDetail, UboDetail};

mod early;
mod mirror;
mod staging;

#[test]
fn unknown_programs_count_as_writing_and_known_read_only_ones_do_not() {
    let mut rw = Rw::default();
    let slot_stage = 3 | (Stage::Fragment as u32) << 24;
    assert!(rw.written(Some(7), slot_stage), "unknown");
    assert!(rw.written(None, slot_stage), "no program (mesh stage)");
    rw.known.insert((7, slot_stage), false);
    assert!(!rw.written(Some(7), slot_stage));
    rw.known.insert((8, slot_stage), true);
    assert!(rw.written(Some(8), slot_stage));
}

#[test]
fn the_written_ranges_cover_stores_copies_and_learned_pools() {
    use crate::detail::{CopyDetail, DispatchDetail, UboDetail};
    let ubo = |address, size, slot| UboDetail {
        stage: Stage::Compute,
        slot,
        address,
        size,
        snapshot: None,
    };
    let detail = FrameDetail {
        dispatches: vec![DispatchDetail {
            program: "cs".into(),
            push: None,
            groups: [1, 1, 1],
            indirect: None,
            ubos: Vec::new(),
            sbufs: vec![ubo(0x1000, 64, 0), ubo(0x2000, 64, 1)],
            textures: Vec::new(),
            images: Vec::new(),
        }],
        copies: vec![
            CopyDetail {
                kind: CopyKind::BufferFill,
                dst_addr: 0x3000,
                bytes: 32,
                ..Default::default()
            },
            CopyDetail {
                kind: CopyKind::TextureToTexture,
                dst_addr: 0x9000,
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let mut rw = Rw::default();
    // Slot 1 of the compute program is read only.
    rw.known
        .insert((hash_str("cs"), 1 | (Stage::Compute as u32) << 24), false);
    let r = written_ranges(&detail, &[(0x5000, 0x6000)], &rw);
    assert!(r.contains(&(0x1000, 64)), "unknown slot counts as writing");
    assert!(
        !r.iter().any(|x| x.0 == 0x2000),
        "read only slot is left out"
    );
    assert!(r.contains(&(0x3000, 32)));
    assert!(
        !r.iter().any(|x| x.0 == 0x9000),
        "a texture copy writes no buffer"
    );
    assert!(r.contains(&(0x5000, 0x1000)), "a learned pool is whole");
}

// ----- the mirror -----

use super::TEST_TURN as TURN;
const VA: u64 = nxkit_metal::GUEST_VA_BASE;

struct Guest {
    mem: Arc<GuestMemory>,
    map: AddressMap,
    ctx: Context,
}

fn guest(len: u64) -> Option<Guest> {
    let ctx = Context::system_default().ok()?;
    let mem = Arc::new(GuestMemory::allocate(&ctx, len).ok()?);
    let map = AddressMap::new(mem.host_base(), VA, mem.len()).ok()?;
    FORCE.with(|f| f.set(Some(Mode::Mirror)));
    FAILED.store(false, Ordering::Relaxed);
    attach(&ctx, mem.host_base(), mem.len(), VA);
    // These tests are about the waits for a writer; the late results are tested in `nxkit-metal`.
    Pages::global().set_late(false);
    Some(Guest { mem, map, ctx })
}

fn dispatch(addr: u64, size: u32) -> FrameDetail {
    let ubo = |address, size, slot| UboDetail {
        stage: Stage::Compute,
        slot,
        address,
        size,
        snapshot: None,
    };
    FrameDetail {
        dispatches: vec![DispatchDetail {
            program: "cs".into(),
            push: None,
            groups: [1, 1, 1],
            indirect: None,
            ubos: vec![ubo(addr + 0x8000, 64, 0)],
            sbufs: vec![ubo(addr, size, 0)],
            textures: Vec::new(),
            images: Vec::new(),
        }],
        ..Default::default()
    }
}

fn pools_of(g: &Guest, o: Arc<GuestOverlay>) -> crate::guestpools::GuestPools {
    crate::guestpools::GuestPools::whole(VA, g.mem.len(), g.mem.clone()).with_overlay(o)
}
