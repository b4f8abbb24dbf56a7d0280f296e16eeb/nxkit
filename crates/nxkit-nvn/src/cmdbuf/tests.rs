use super::*;
use crate::mem::VecMem;

const BASE: u64 = 0x7000_0000;

fn setup() -> (CommandBuffer, VecMem) {
    (CommandBuffer::new(), VecMem::new(BASE, 4096))
}

#[test]
fn a_recording_is_bytes_in_the_memory_of_the_game() {
    let (mut cb, mut mem) = setup();
    cb.add_command_memory(BASE + 0x100, 256).unwrap();
    cb.begin_recording().unwrap();
    cb.write(&mut mem, Record::Draw).unwrap();
    cb.write(&mut mem, Record::Dispatch).unwrap();
    let r = cb.end_recording(&mut mem).unwrap();
    assert_eq!(r.handle, CommandHandle(BASE + 0x100));
    assert_eq!(r.spans.len(), 1);
    assert_eq!(r.spans[0], BASE + 0x100..BASE + 0x118);
    assert_eq!(cb.used(), 24);
    assert_eq!(cb.free(), 256 - 24);
    assert_eq!(
        Record::read(&mem, BASE + 0x108).unwrap().0,
        Record::Dispatch
    );
    assert!(!cb.is_recording());
}

#[test]
fn an_empty_recording_is_only_its_end() {
    let (mut cb, mut mem) = setup();
    cb.add_command_memory(BASE, 64).unwrap();
    cb.begin_recording().unwrap();
    let r = cb.end_recording(&mut mem).unwrap();
    assert_eq!(r.spans[0], BASE..BASE + 8);
}

#[test]
fn without_room_nothing_is_written_until_the_game_adds_memory() {
    let (mut cb, mut mem) = setup();
    cb.add_command_memory(BASE, 64).unwrap();
    cb.begin_recording().unwrap();
    // 3 draws of 8 bytes and the room of 16 bytes for a jump fill 64 bytes less 24 bytes
    let mut n = 0;
    while cb.write(&mut mem, Record::Draw).is_ok() {
        n += 1;
    }
    assert_eq!(n, 6);
    let used = cb.used();
    assert_eq!(
        cb.write(&mut mem, Record::Draw),
        Err(NvnError::OutOfCommandMemory { need: 24 })
    );
    assert_eq!(cb.used(), used);
    cb.add_command_memory(BASE + 0x200, 128).unwrap();
    assert_eq!(cb.used(), 0);
    cb.write(&mut mem, Record::Dispatch).unwrap();
    let r = cb.end_recording(&mut mem).unwrap();
    assert_eq!(r.handle.0, BASE);
    assert_eq!(r.spans.len(), 2);
    assert_eq!(r.spans[1].start, BASE + 0x200);
    // the jump leads from the first block to the second
    let at = r.spans[0].end - u64::from(JUMP_BYTES);
    assert_eq!(
        Record::read(&mem, at).unwrap().0,
        Record::Jump(BASE + 0x200)
    );
}

#[test]
fn the_end_record_may_take_the_last_bytes() {
    let (mut cb, mut mem) = setup();
    cb.add_command_memory(BASE, 64).unwrap();
    cb.begin_recording().unwrap();
    for _ in 0..6 {
        cb.write(&mut mem, Record::Draw).unwrap();
    }
    assert_eq!(cb.free(), 16);
    cb.end_recording(&mut mem).unwrap();
    assert_eq!(cb.free(), 8);
}

#[test]
fn wrong_calls_are_errors() {
    let (mut cb, mut mem) = setup();
    assert!(check_request(BASE, 32).is_err());
    assert!(check_request(BASE + 4, 64).is_err());
    assert!(check_request(BASE, 100).is_err());
    assert!(cb.add_command_memory(BASE + 4, 64).is_err());
    assert!(cb.add_command_memory(BASE, 60).is_err());
    assert!(cb.begin_recording().is_ok());
    assert!(cb.begin_recording().is_err());
    assert_eq!(
        cb.write(&mut mem, Record::Draw),
        Err(NvnError::OutOfCommandMemory { need: 24 })
    );
    let mut idle = CommandBuffer::new();
    assert!(idle.write(&mut mem, Record::Draw).is_err());
}

#[test]
fn memory_outside_the_mapping_is_an_error_and_changes_nothing() {
    let (mut cb, mut mem) = setup();
    cb.add_command_memory(BASE + 4096 - 32, 64).unwrap();
    cb.begin_recording().unwrap();
    for _ in 0..4 {
        cb.write(&mut mem, Record::Draw).unwrap();
    }
    assert!(cb.write(&mut mem, Record::Draw).is_err());
    assert_eq!(cb.used(), 32);
}

#[test]
fn a_writer_outside_the_buffer_commits_its_records() {
    let (mut cb, mut mem) = setup();
    assert_eq!(cb.cursor().pos, cb.cursor().limit);
    cb.add_command_memory(BASE + 0x100, 128).unwrap();
    cb.begin_recording().unwrap();
    let c = cb.cursor();
    assert_eq!(c.pos, BASE + 0x100);
    assert_eq!(c.limit, BASE + 0x100 + 128 - u64::from(JUMP_BYTES));
    let rec = Record::Draw.encode().unwrap();
    mem.write(c.pos, rec.as_bytes()).unwrap();
    mem.write(c.pos + 8, rec.as_bytes()).unwrap();
    cb.commit(c.pos + 16);
    assert_eq!(cb.used(), 16);
    assert_eq!(cb.cursor().pos, c.pos + 16);
    cb.commit(c.pos);
    assert_eq!(cb.used(), 16);
    let r = cb.end_recording(&mut mem).unwrap();
    assert_eq!(r.handle, CommandHandle(BASE + 0x100));
    assert_eq!(r.spans, vec![BASE + 0x100..BASE + 0x100 + 24]);
}

#[test]
fn a_recording_begun_without_memory_starts_in_the_first_block_that_the_game_adds() {
    let (mut cb, mut mem) = setup();
    cb.begin_recording().unwrap();
    assert_eq!(
        cb.write(&mut mem, Record::Draw),
        Err(NvnError::OutOfCommandMemory { need: 24 })
    );
    // the memory callback of the game adds the first block during the recording
    cb.add_command_memory(BASE + 0x200, 128).unwrap();
    assert_eq!(cb.cursor().pos, BASE + 0x200);
    cb.write(&mut mem, Record::Draw).unwrap();
    let r = cb.end_recording(&mut mem).unwrap();
    assert_eq!(r.handle, CommandHandle(BASE + 0x200));
    assert_eq!(r.spans, vec![BASE + 0x200..BASE + 0x210]);
    assert_eq!(Record::read(&mem, BASE + 0x200).unwrap().0, Record::Draw);
}

#[test]
fn an_encoded_record_goes_the_same_way_as_a_record() {
    let (mut cb, mut mem) = setup();
    cb.add_command_memory(BASE, 64).unwrap();
    cb.begin_recording().unwrap();
    let e = Record::Dispatch.encode().unwrap();
    cb.write_encoded(&mut mem, e.as_bytes(), false).unwrap();
    assert_eq!(Record::read(&mem, BASE).unwrap().0, Record::Dispatch);
}

#[test]
fn the_refusal_names_the_rule_and_the_values() {
    let e = check_request(BASE + 4, 512).unwrap_err();
    assert_eq!(
        e,
        NvnError::BadCommandMemory {
            start: BASE + 4,
            len: 512,
            rule: "the start is not a multiple of 8"
        }
    );
    assert!(e.to_string().contains("0x70000004"));
    assert!(e.to_string().contains("512 bytes"));
    assert!(check_request(BASE, 64).is_ok());
    assert!(check_request(BASE, 512).is_ok());
}

#[test]
fn the_clip_cuts_to_the_pool_end_and_rounds_down_to_8() {
    assert_eq!(clip_block(512, 4096), 512);
    assert_eq!(clip_block(512, 200), 200);
    assert_eq!(clip_block(512, 60), 56);
    assert_eq!(clip_block(512, 63), 56);
    assert_eq!(clip_block(512, 7), 0);
    assert_eq!(clip_block(512, 0), 0);
    assert_eq!(clip_block(64, 64), 64);
}

#[test]
fn a_block_below_the_minimum_holds_what_fits() {
    let (mut cb, mut mem) = setup();
    // 24 bytes: one record and the room of a jump, or the end record
    cb.add_command_memory(BASE, 24).unwrap();
    cb.begin_recording().unwrap();
    cb.write(&mut mem, Record::Draw).unwrap();
    assert_eq!(cb.cursor().limit, BASE + 8);
    assert_eq!(
        cb.write(&mut mem, Record::Draw),
        Err(NvnError::OutOfCommandMemory { need: 24 })
    );
    let r = cb.end_recording(&mut mem).unwrap();
    assert_eq!(r.spans, vec![BASE..BASE + 16]);
    assert_eq!(Record::read(&mem, BASE + 8).unwrap().0, Record::End);
}

#[test]
fn a_block_of_8_bytes_holds_only_the_end_record() {
    let (mut cb, mut mem) = setup();
    cb.add_command_memory(BASE, 8).unwrap();
    cb.begin_recording().unwrap();
    assert_eq!(
        cb.write(&mut mem, Record::Draw),
        Err(NvnError::OutOfCommandMemory { need: 24 })
    );
    let r = cb.end_recording(&mut mem).unwrap();
    assert_eq!(r.handle, CommandHandle(BASE));
    assert_eq!(r.spans, vec![BASE..BASE + 8]);
}

#[test]
fn a_block_of_0_bytes_acts_as_a_full_block() {
    let (mut cb, mut mem) = setup();
    cb.add_command_memory(BASE + 0x100, 0).unwrap();
    assert_eq!(cb.free(), 0);
    assert_eq!(cb.cursor().pos, cb.cursor().limit);
    cb.begin_recording().unwrap();
    assert_eq!(
        cb.write(&mut mem, Record::Draw),
        Err(NvnError::OutOfCommandMemory { need: 24 })
    );
    assert_eq!(cb.used(), 0);
    // the game adds memory, the record goes to it and no jump is written in the empty block
    cb.add_command_memory(BASE + 0x200, 128).unwrap();
    cb.write(&mut mem, Record::Draw).unwrap();
    let r = cb.end_recording(&mut mem).unwrap();
    assert_eq!(r.handle, CommandHandle(BASE + 0x200));
    assert_eq!(r.spans, vec![BASE + 0x200..BASE + 0x200 + 16]);
    assert_eq!(Record::read(&mem, BASE + 0x200).unwrap().0, Record::Draw);
}

#[test]
fn the_end_record_of_a_block_of_0_bytes_goes_to_the_next_block() {
    let (mut cb, mut mem) = setup();
    cb.add_command_memory(BASE, 0).unwrap();
    cb.begin_recording().unwrap();
    assert_eq!(
        cb.end_recording(&mut mem),
        Err(NvnError::OutOfCommandMemory { need: 8 })
    );
    cb.add_command_memory(BASE + 0x40, 64).unwrap();
    let r = cb.end_recording(&mut mem).unwrap();
    assert_eq!(r.handle, CommandHandle(BASE + 0x40));
}

#[test]
fn a_recording_that_starts_in_a_block_tail_without_room_for_a_jump_starts_in_the_next_block() {
    let (mut cb, mut mem) = setup();
    cb.add_command_memory(BASE, 64).unwrap();
    cb.begin_recording().unwrap();
    for _ in 0..6 {
        cb.write(&mut mem, Record::Draw).unwrap();
    }
    cb.end_recording(&mut mem).unwrap();
    // 8 bytes are left, the next recording has no room for a jump
    assert_eq!(cb.free(), 8);
    cb.begin_recording().unwrap();
    cb.add_command_memory(BASE + 0x100, 64).unwrap();
    cb.write(&mut mem, Record::Dispatch).unwrap();
    let r = cb.end_recording(&mut mem).unwrap();
    assert_eq!(r.handle, CommandHandle(BASE + 0x100));
}

#[test]
fn a_recording_overflows_out_of_a_short_block_into_the_next_and_decodes_back() {
    let (mut cb, mut mem) = setup();
    // 56 bytes, as a clip of 512 at 56 bytes before the pool end gives
    let n = clip_block(512, 59);
    assert_eq!(n, 56);
    cb.add_command_memory(BASE, n).unwrap();
    cb.begin_recording().unwrap();
    // 5 records and a jump fill a block of 56 bytes, so 11 records leave two blocks and end in the third
    let recs: Vec<Record> = (0..11)
        .map(|i| {
            if i % 2 == 0 {
                Record::Draw
            } else {
                Record::Dispatch
            }
        })
        .collect();
    let mut sent = 0;
    let mut blocks = 0;
    while sent < recs.len() {
        match cb.write(&mut mem, recs[sent]) {
            Ok(()) => sent += 1,
            Err(NvnError::OutOfCommandMemory { .. }) => {
                blocks += 1;
                cb.add_command_memory(BASE + 0x100 * blocks, 56).unwrap();
            }
            Err(e) => panic!("{e}"),
        }
    }
    let r = cb.end_recording(&mut mem).unwrap();
    assert_eq!(r.handle, CommandHandle(BASE));
    assert_eq!(r.spans.len(), 3);
    assert_eq!(r.spans[0], BASE..BASE + 56);
    assert_eq!(r.spans[1], BASE + 0x100..BASE + 0x100 + 56);
    assert_eq!(r.spans[2], BASE + 0x200..BASE + 0x200 + 16);
    // follow the jumps from the handle and read the records back
    let (mut at, mut got) = (r.handle.0, Vec::new());
    loop {
        match Record::read(&mem, at).unwrap().0 {
            Record::Jump(to) => at = to,
            Record::End => break,
            rec => {
                got.push(rec);
                at += 8;
            }
        }
    }
    assert_eq!(got, recs);
}

#[test]
fn a_too_small_block_added_during_a_recording_is_passed_over() {
    let (mut cb, mut mem) = setup();
    cb.add_command_memory(BASE, 64).unwrap();
    cb.begin_recording().unwrap();
    for _ in 0..6 {
        cb.write(&mut mem, Record::Draw).unwrap();
    }
    assert!(cb.write(&mut mem, Record::Draw).is_err());
    cb.add_command_memory(BASE + 0x80, 16).unwrap();
    assert!(cb.write(&mut mem, Record::Draw).is_err());
    cb.add_command_memory(BASE + 0x100, 64).unwrap();
    cb.write(&mut mem, Record::Draw).unwrap();
    let r = cb.end_recording(&mut mem).unwrap();
    assert_eq!(r.spans[1].start, BASE + 0x100);
}

#[test]
fn the_limit_of_a_short_block_is_never_below_the_position() {
    let mut cb = CommandBuffer::new();
    for len in [0, 8] {
        cb.add_command_memory(BASE, len).unwrap();
        let c = cb.cursor();
        assert_eq!((c.pos, c.limit), (BASE, BASE));
    }
    cb.add_command_memory(BASE, 16).unwrap();
    assert_eq!(cb.cursor().limit, BASE);
    cb.add_command_memory(BASE, 24).unwrap();
    assert_eq!(cb.cursor().limit, BASE + 8);
}
