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

/// The smallest block of command memory.
pub const MIN_BLOCK_BYTES: u64 = 64;

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
    /// a record that does not fit goes to. The counter of `used` starts again, as in NVN and in nxnative.
    pub fn add_command_memory(&mut self, start: u64, len: u64) -> Result<()> {
        if len < MIN_BLOCK_BYTES || !len.is_multiple_of(8) || !start.is_multiple_of(8) {
            return Err(NvnError::FieldRange("command memory block"));
        }
        let end = start
            .checked_add(len)
            .ok_or(NvnError::FieldRange("command memory block"))?;
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
            let next = self
                .blocks
                .get(self.current + 1)
                .filter(|b| b.end - b.start >= need)
                .cloned();
            let Some(next) = next
                .filter(|_| self.pos + u64::from(JUMP_BYTES) <= self.blocks[self.current].end)
            else {
                return Err(NvnError::OutOfCommandMemory { need: need as u32 });
            };
            let jump = Record::Jump(next.start).encode()?;
            self.put(mem, jump.as_bytes())?;
            if let Some(from) = self.open_from.take() {
                self.spans.push(from..self.pos);
            }
            self.current += 1;
            self.pos = next.start;
        }
        self.put(mem, bytes)
    }

    /// Where a writer that bypasses [`CommandBuffer::write`] puts its next record. `limit` is the end of the room for a
    /// record that is not the end record: the block end less the room of a jump. A command buffer without a block
    /// gives `limit == pos`, so no record fits.
    pub fn cursor(&self) -> Cursor {
        let limit = self
            .blocks
            .get(self.current)
            .map_or(self.pos, |b| b.end.saturating_sub(u64::from(JUMP_BYTES)));
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
        assert!(cb.add_command_memory(BASE, 32).is_err());
        assert!(cb.add_command_memory(BASE + 4, 64).is_err());
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
}
