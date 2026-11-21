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
    if !requested.is_multiple_of(4) {
        return Err(bad("the requested length is not a multiple of 4"));
    }
    if !start.is_multiple_of(4) {
        return Err(bad("the start is not a multiple of 4"));
    }
    Ok(())
}

/// The part of a block that the host records into: NVN aligns command memory to 4 bytes, the records of the host are 8
/// byte aligned, so a start that is only 4-aligned moves up to the next multiple of 8 and the length shrinks by those
/// bytes and is rounded down to a multiple of 8. Octopath Traveler (Unreal Engine 4.18) adds such blocks.
pub fn align_block(start: u64, len: u64) -> (u64, u64) {
    let at = start.next_multiple_of(8);
    (at, len.saturating_sub(at - start) & !7)
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
        // A recording that has no block yet (the game began it before it gave memory, and the memory callback gives
        // the first block) starts in the new block, as a recording that began after the block was added.
        if self.recording && !self.blocks.is_empty() {
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
            .is_some_and(|b| self.pos >= b.start && self.pos + need <= b.end)
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
mod tests;
