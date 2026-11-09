//! A command buffer that records into the command memory of the game.
//!
//! The behaviour follows NVN. The game gives memory blocks with `add_command_memory`.
//! A recording starts with `begin_recording` and ends with `end_recording`, which gives the handle.
//! The handle is the host address of the first record. A record that does not fit goes to the next block behind a `Jump`.
//! When no block is left, `write` fails with `OutOfCommandMemory` and writes nothing. The game then adds memory and writes again.
//! This is the memory callback of NVN.
//!
//! The command buffer keeps no NVN state. It only writes records. The decoder in [`crate::decode`] follows the state.

use std::ops::Range;

use crate::error::{NvnError, Result};
use crate::mem::GuestMem;
use crate::record::{JUMP_BYTES, Record};

/// The smallest block that a game may ask for. The rule is on the request of the game, not on the block that the host
/// uses: a block that the end of its pool cut short can be smaller (see [`clip_block`]).
pub const MIN_BLOCK_BYTES: u64 = 64;

/// The rules of `nvnCommandBufferAddCommandMemory` on what the game asked for: the host address of the start and the
/// requested length. The refusal names the rule and carries both values.
pub fn check_request(start: u64, requested: u64) -> Result<()> {
    let bad = |rule| NvnError::BadCommandMemory {
        start,
        len: requested,
        rule,
    };
    if requested < MIN_BLOCK_BYTES {
        return Err(bad("the request is smaller than the minimum block"));
    }
    if !requested.is_multiple_of(8) {
        return Err(bad("the requested length is not a multiple of 8"));
    }
    if !start.is_multiple_of(8) {
        return Err(bad("the start is not a multiple of 8"));
    }
    Ok(())
}

/// The bytes of a requested block that the host uses when only `available` bytes are left in the pool. Games ask for
/// more than is left near the pool end and the recorder writes only inside the block, so the block is cut to the pool
/// end and rounded down to a multiple of 8. The result can be below [`MIN_BLOCK_BYTES`], also 0. Such a block holds
/// what fits, and a record that does not fit goes the way it goes when a block is full.
pub fn clip_block(requested: u64, available: u64) -> u64 {
    requested.min(available) & !7
}

/// The handle of a finished recording. It is the host address of its first record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommandHandle(pub u64);

/// A finished recording and the host address ranges that its records occupy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recording {
    /// The handle.
    pub handle: CommandHandle,
    /// The memory that holds the records, one range for each block that the recording used.
    pub spans: Vec<Range<u64>>,
}

/// The write position of a command buffer, for a writer that puts records straight into command memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    /// The host address of the next record.
    pub pos: u64,
    /// A record that is not the end record fits when `pos + size <= limit`.
    pub limit: u64,
}

/// A command buffer.
#[derive(Debug, Default)]
pub struct CommandBuffer {
    blocks: Vec<Range<u64>>,
    current: usize,
    pos: u64,
    used: u64,
    recording: bool,
    begin: Option<u64>,
    open_from: Option<u64>,
    spans: Vec<Range<u64>>,
}

impl CommandBuffer {
    /// A command buffer without memory.
    pub fn new() -> Self {
        Self::default()
    }

    /// `nvnCommandBufferAddCommandMemory`. The block is at a host address.
    /// Between recordings the block is where the next recording starts. During a recording it is the next block, which
    /// a record that does not fit goes to. The counter of `used` starts again, as in NVN and in the engine.
    pub fn add_command_memory(&mut self, start: u64, len: u64) -> Result<()> {
        let bad = |rule| NvnError::BadCommandMemory { start, len, rule };
        if !start.is_multiple_of(8) {
            return Err(bad("the start is not a multiple of 8"));
        }
        if !len.is_multiple_of(8) {
            return Err(bad("the length is not a multiple of 8"));
        }
        let end = start
            .checked_add(len)
            .ok_or_else(|| bad("the block ends past the address space"))?;
        if self.recording {
            self.blocks.push(start..end);
        } else {
            self.blocks.clear();
            self.blocks.push(start..end);
            self.current = 0;
            self.pos = start;
        }
        self.used = 0;
        Ok(())
    }

    /// `nvnCommandBufferBeginRecording`.
    pub fn begin_recording(&mut self) -> Result<()> {
        if self.recording {
            return Err(NvnError::BadRecord("recording is already open"));
        }
        self.recording = true;
        self.begin = None;
        self.open_from = None;
        self.spans.clear();
        Ok(())
    }

    /// True between `begin_recording` and `end_recording`.
    pub fn is_recording(&self) -> bool {
        self.recording
    }

    /// `nvnCommandBufferGetCommandMemoryUsed`: the bytes written since the last block was added.
    pub fn used(&self) -> u64 {
        self.used
    }

    /// `nvnCommandBufferGetCommandMemoryFree`: the bytes that are left in the current block.
    pub fn free(&self) -> u64 {
        self.blocks
            .get(self.current)
            .map_or(0, |b| b.end.saturating_sub(self.pos))
    }

    fn fits(&self, need: u64) -> bool {
        self.blocks
            .get(self.current)
            .is_some_and(|b| self.pos + need <= b.end)
    }

    fn put(&mut self, mem: &mut impl GuestMem, bytes: &[u8]) -> Result<()> {
        mem.write(self.pos, bytes)?;
        self.begin.get_or_insert(self.pos);
        self.open_from.get_or_insert(self.pos);
        self.pos += bytes.len() as u64;
        self.used += bytes.len() as u64;
        Ok(())
    }

    /// Writes one record. A failure leaves the command buffer as it was.
    pub fn write(&mut self, mem: &mut impl GuestMem, rec: Record) -> Result<()> {
        let e = rec.encode()?;
        self.write_encoded(mem, e.as_bytes(), rec == Record::End)
    }

    /// Writes one record that is already encoded. `is_end` is true for the end record, which needs no room for a
    /// jump behind it. A failure leaves the command buffer as it was.
    pub fn write_encoded(
        &mut self,
        mem: &mut impl GuestMem,
        bytes: &[u8],
        is_end: bool,
    ) -> Result<()> {
        if !self.recording {
            return Err(NvnError::BadRecord("no recording is open"));
        }
        let need = bytes.len() as u64 + if is_end { 0 } else { u64::from(JUMP_BYTES) };
        if !self.fits(need) {
            let cur_end = self.blocks.get(self.current).map(|b| b.end);
            // The first block after the current one that holds the record and a jump. A block before it is too small for
            // the record, was never jumped to and is left unused.
            let next = (self.current + 1..self.blocks.len())
                .find(|&i| self.blocks[i].end - self.blocks[i].start >= need);
            let (Some(cur_end), Some(next)) = (cur_end, next) else {
                return Err(NvnError::OutOfCommandMemory { need: need as u32 });
            };
            let start = self.blocks[next].start;
            if self.pos + u64::from(JUMP_BYTES) <= cur_end {
                let jump = Record::Jump(start).encode()?;
                self.put(mem, jump.as_bytes())?;
                if let Some(from) = self.open_from.take() {
                    self.spans.push(from..self.pos);
                }
            } else if self.begin.is_some() {
                // A record is only written with room for a jump behind it, so this does not happen. A block that
                // cannot take a jump cannot be left.
                return Err(NvnError::OutOfCommandMemory { need: need as u32 });
            }
            // Else nothing of this recording is in the current block, so no record leads into it. A block with less
            // than the room of a jump (the end of a short block, or a block of 0 bytes) is dropped without a jump and
            // the recording starts in the next block.
            self.current = next;
            self.pos = start;
        }
        self.put(mem, bytes)
    }

    /// Where a writer that bypasses [`CommandBuffer::write`] puts its next record. `limit` is the end of the room for a
    /// record that is not the end record: the block end less the room of a jump, and never below `pos`. A command
    /// buffer without a block, and a block with less room than a jump, give `limit == pos`, so no record fits.
    pub fn cursor(&self) -> Cursor {
        let limit = self
            .blocks
            .get(self.current)
            .map_or(self.pos, |b| b.end.saturating_sub(u64::from(JUMP_BYTES)))
            .max(self.pos);
        Cursor {
            pos: self.pos,
            limit,
        }
    }

    /// Takes records that a writer put straight into the current block, from [`Cursor::pos`] up to `pos`. The
    /// first records of a recording set its handle and the start of its first span.
    pub fn commit(&mut self, pos: u64) {
        if pos <= self.pos {
            return;
        }
        self.begin.get_or_insert(self.pos);
        self.open_from.get_or_insert(self.pos);
        self.used += pos - self.pos;
        self.pos = pos;
    }

    /// `nvnCommandBufferEndRecording`. It writes the end record and gives the finished recording.
    pub fn end_recording(&mut self, mem: &mut impl GuestMem) -> Result<Recording> {
        self.write(mem, Record::End)?;
        self.recording = false;
        if let Some(from) = self.open_from.take() {
            self.spans.push(from..self.pos);
        }
        let handle = CommandHandle(self.begin.unwrap_or(self.pos));
        Ok(Recording {
            handle,
            spans: std::mem::take(&mut self.spans),
        })
    }
}

#[cfg(test)]
mod tests {
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
}
