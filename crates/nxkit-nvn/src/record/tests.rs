//! Tests of the record format.

use super::*;
use crate::mem::VecMem;

fn all() -> Vec<Record> {
    let mut color = [0u64; MAX_COLOR];
    color[0] = 0x15_0000_1000;
    color[3] = 0x15_0000_9000;
    vec![
        Record::End,
        Record::Jump(0x7000_0000),
        Record::Call(0x7000_1230),
        Record::SetTargets {
            color,
            depth: 0x15_0001_0000,
        },
        Record::SetTargets {
            color: [0; MAX_COLOR],
            depth: 0x15_0001_0000,
        },
        Record::SetScissor { full: true },
        Record::ClearColor { slot: 7 },
        Record::ClearDepthStencil,
        Record::Discard {
            color: 0x81,
            depth: true,
        },
        Record::BindState(DrawState {
            color_write: 0x0f,
            blend: true,
            depth_test: true,
            depth_write: false,
            stencil_test: true,
        }),
        Record::BindTexture {
            slot: 127,
            handle: 42 | 3 << 20,
        },
        Record::BindImage {
            slot: 0,
            handle: NO_HANDLE,
        },
        Record::BindTexturesDeferred {
            first: 96,
            count: 32,
            src: 0x7000_4000,
            tex_base: 7,
            samp_base: 3,
        },
        Record::Draw,
        Record::Dispatch,
        Record::Copy {
            src: 0,
            dst: 0x15_0000_2000,
        },
        Record::Downsample {
            src: 0x15_0000_2000,
            dst: 0x15_0000_3000,
        },
        Record::Barrier(0x42),
        Record::Sync,
        Record::Present(0x15_0000_2000),
        Record::CpuRead(0x15_0000_2000),
        Record::Ext(Ext::new(ExtCode::Draw, 1).u32(4).u32(0).u32(36).u32(0)),
        Record::Ext(Ext::new(ExtCode::Setting, 0)),
        Record::Ext(Ext::new(ExtCode::Push, 3).bytes(&[7; 112])),
    ]
}

#[test]
fn every_record_round_trips_through_memory() {
    let mut m = VecMem::new(0x7000_0000, 4096);
    for r in all() {
        let e = r.encode().unwrap();
        assert_eq!(e.len() % 8, 0);
        assert!(e.as_bytes().len() <= MAX_RECORD_BYTES);
        m.write(0x7000_0100, e.as_bytes()).unwrap();
        assert_eq!(
            Record::read(&m, 0x7000_0100).unwrap(),
            (r, e.len()),
            "{r:?}"
        );
    }
}

#[test]
fn records_are_small() {
    let size = |r: Record| r.encode().unwrap().len();
    assert_eq!(size(Record::Draw), 8);
    assert_eq!(size(Record::Dispatch), 8);
    assert_eq!(size(Record::ClearColor { slot: 0 }), 8);
    assert_eq!(size(Record::BindState(DrawState::default())), 8);
    assert_eq!(size(Record::BindTexture { slot: 0, handle: 0 }), 16);
    assert_eq!(size(Record::Call(0)), 16);
    assert_eq!(size(Record::Jump(0)), JUMP_BYTES);
    let full = Record::SetTargets {
        color: [1; MAX_COLOR],
        depth: 1,
    };
    assert_eq!(size(full), 88);
    let widest = Record::Ext(Ext::new(ExtCode::Push, 0).bytes(&[0; 112]));
    assert_eq!(size(widest) as usize, MAX_RECORD_BYTES);
    assert_eq!(
        size(Record::Ext(Ext::new(ExtCode::Draw, 0).u32(1).u32(2).u32(3))),
        24
    );
    assert!(all().iter().all(|r| size(*r) as usize <= MAX_RECORD_BYTES));
}

#[test]
fn a_deferred_bind_reads_the_handle_that_memory_holds_now() {
    let mut m = VecMem::new(0x7000_0000, 4096);
    let at = 0x7000_0200;
    m.write(at + 8, &(5u64 | 2 << 20).to_le_bytes()).unwrap();
    assert_eq!(
        deferred_handle(&m, at, 1, 100, 10).unwrap(),
        (at + 8, 5 | 2 << 20, 105 | 12 << 20)
    );
    m.write(at + 8, &(9u64 | 2 << 20).to_le_bytes()).unwrap();
    assert_eq!(
        deferred_handle(&m, at, 1, 100, 10).unwrap().2,
        109 | 12 << 20
    );
    assert_eq!(deferred_handle(&m, at, 0, 100, 10).unwrap().2, 0);
}

#[test]
fn values_that_do_not_fit_are_errors() {
    assert!(
        Record::BindTexture {
            slot: 128,
            handle: 0
        }
        .encode()
        .is_err()
    );
    assert!(Record::ClearColor { slot: 8 }.encode().is_err());
    assert!(Record::Barrier(0x100).encode().is_err());
    let too_many =
        (0..=crate::ext::MAX_EXT_WORDS).fold(Ext::new(ExtCode::Push, 0), |e, i| e.u32(i as u32));
    assert!(Record::Ext(too_many).encode().is_err());
}

#[test]
fn bad_records_in_guest_memory_are_errors() {
    let mut m = VecMem::new(0, 256);
    let header = |op: u16, size: u32| {
        let mut h = [0u8; 8];
        h[..2].copy_from_slice(&op.to_le_bytes());
        h[4..].copy_from_slice(&size.to_le_bytes());
        h
    };
    // a size of 0, a size that is not a multiple of 8, a size that is too large,
    // an unknown operation and a record with a payload that is too short
    for (op, size) in [
        (OP_DRAW, 0),
        (OP_DRAW, 12),
        (OP_DRAW, 4096),
        (FIRST_EXT_OP - 1, 8),
        (FIRST_EXT_OP + 200, 8),
        (OP_COPY, 8),
    ] {
        m.write(0, &header(op, size)).unwrap();
        assert!(Record::read(&m, 0).is_err(), "{op} {size}");
    }
}
