//! The HID shared memory in the Horizon layout, for games that read it directly instead of calling `nn::hid`.
//!
//! The npad section follows what the SDK of Breath of the Wild reads: entry `i` at 0x9A00 + 0x5000 * i, with a style
//! set word at its start and ring buffers (LIFOs) of states after it. Each LIFO has a header and 17 entries. An
//! entry is an even sequence number (odd while it is written) and the state. This was checked on 2026-11-07 against
//! `nn::hid::detail::NpadSharedMemoryAccessor` in the SDK module of the game. The offsets of the debug pad, touch
//! screen, mouse and keyboard sections are from memory of the layout and unverified.
//!
//! The memory is written by `fill`, which the host calls on each display tick through [`crate::tick`].

use nxkit_abi::guest::{self, Fault};
use nxkit_abi::kernel;

use crate::npad::{ID_HANDHELD, ID_OTHER, Layout, style};
use crate::state::{Input, six_axis_bytes};

/// The size of the shared memory.
pub(crate) const SIZE: u64 = 0x40000;

/// The result of a call that needs the kernel when the host has not set one.
pub(crate) const NO_KERNEL: u32 = 0xffff_ffff;

/// The number of entries of a LIFO.
const LIFO_ENTRIES: i64 = 17;
/// The size of a LIFO header.
const LIFO_HEADER: u64 = 0x20;

/// Section offsets.
mod at {
    /// Unverified.
    pub(super) const MOUSE: u64 = 0x3400;
    /// Unverified.
    pub(super) const KEYBOARD: u64 = 0x3800;
    pub(super) const NPAD: u64 = 0x9a00;
    pub(super) const NPAD_STRIDE: u64 = 0x5000;
    /// Offsets inside an npad entry of the state LIFOs.
    pub(super) const FULL_KEY: u64 = 0x28;
    pub(super) const HANDHELD: u64 = 0x378;
    /// Offsets inside an npad entry of the six-axis LIFOs.
    pub(super) const SIX_FULL_KEY: u64 = 0x1758;
    pub(super) const SIX_HANDHELD: u64 = 0x1e60;
}

/// The shared memory block that a game got.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SharedMemory {
    /// The handle the game maps.
    pub(crate) handle: u32,
    /// Where the kernel mapped it in guest memory.
    pub(crate) addr: u64,
}

impl SharedMemory {
    /// Creates the block through the kernel of the host.
    pub(crate) fn create() -> Result<SharedMemory, u32> {
        let kernel = kernel::kernel().ok_or(NO_KERNEL)?;
        let (handle, addr) = kernel.create_shared_memory(SIZE)?;
        Ok(SharedMemory { handle, addr })
    }
}

/// The index of an npad id in the shared memory: ids 0 to 7 are 0 to 7, handheld is 8 and `Other` is 9.
pub(crate) fn npad_index(id: u32) -> Option<u64> {
    match id {
        0..=7 => Some(u64::from(id)),
        ID_HANDHELD => Some(8),
        ID_OTHER => Some(9),
        _ => None,
    }
}

/// Pushes one entry into the LIFO at `base`. `state` has its sampling number at `sampling_at`. The number is set
/// to the one after the newest entry's.
fn push_lifo(base: u64, state: &mut [u8], sampling_at: usize, timestamp: i64) -> Result<(), Fault> {
    let entry_size = 8 + state.len() as u64;
    let last = guest::read_i64(base + 0x10)?.clamp(0, LIFO_ENTRIES - 1);
    let count = guest::read_i64(base + 0x18)?.clamp(0, LIFO_ENTRIES);
    let entry = |i: i64| base + LIFO_HEADER + i as u64 * entry_size;
    let previous = if count > 0 {
        guest::read_i64(entry(last) + 8 + sampling_at as u64)?
    } else {
        0
    };
    let sampling = previous.wrapping_add(1);
    state[sampling_at..sampling_at + 8].copy_from_slice(&sampling.to_le_bytes());
    let next = (last + 1) % LIFO_ENTRIES;
    let at = entry(next);
    let number = sampling.wrapping_mul(2) as u64;
    guest::write_u64(at, number | 1)?;
    guest::write(at + 8, state)?;
    guest::write_u64(at, number)?;
    guest::write_i64(base, timestamp)?;
    guest::write_i64(base + 8, LIFO_ENTRIES)?;
    guest::write_i64(base + 0x10, next)?;
    guest::write_i64(base + 0x18, (count + 1).min(LIFO_ENTRIES))
}

/// Writes the current input into the shared memory: one new entry in the LIFO of every npad that has a controller,
/// and in the keyboard and mouse LIFOs.
pub(crate) fn fill(input: &mut Input) -> Result<(), Fault> {
    let Some(shm) = input.shm else {
        return Ok(());
    };
    let layout = input.layout();
    let timestamp = input.now_ns() as i64;
    let ids = (0..8).chain([ID_HANDHELD, ID_OTHER]);
    for id in ids {
        let Some(index) = npad_index(id) else {
            continue;
        };
        let entry = shm.addr + at::NPAD + at::NPAD_STRIDE * index;
        let player = layout.player_of(id);
        let styles = Layout::style_for(id, player);
        guest::write_u32(entry, styles)?;
        let Some(player) = player else { continue };
        for (s, lifo, six) in [
            (style::FULL_KEY, at::FULL_KEY, at::SIX_FULL_KEY),
            (style::HANDHELD, at::HANDHELD, at::SIX_HANDHELD),
        ] {
            if styles & s == 0 {
                continue;
            }
            let mut state = input.npad_state(id, s).to_bytes(0);
            push_lifo(entry + lifo, &mut state, 0, timestamp)?;
            let mut axis = six_axis_bytes(&input.six_axis(player), 0);
            push_lifo(entry + six, &mut axis, 8, timestamp)?;
        }
    }
    if input.config.keyboard {
        let mut keyboard = input.keyboard_state();
        push_lifo(shm.addr + at::KEYBOARD, &mut keyboard, 0, timestamp)?;
        let mut mouse = input.mouse_state();
        push_lifo(shm.addr + at::MOUSE, &mut mouse, 0, timestamp)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::testutil::guest_memory;

    fn sampling_of(entry_at: u64) -> i64 {
        guest::read_i64(entry_at + 8).unwrap_or(-1)
    }

    #[test]
    fn the_index_follows_the_sdk() {
        assert_eq!(npad_index(0), Some(0));
        assert_eq!(npad_index(7), Some(7));
        assert_eq!(npad_index(ID_HANDHELD), Some(8));
        assert_eq!(npad_index(ID_OTHER), Some(9));
        assert_eq!(npad_index(9), None);
    }

    #[test]
    fn a_pushed_entry_is_even_and_counted() {
        let (base, _guard) = guest_memory();
        let mut state = [0u8; 0x28];
        for round in 1..=19i64 {
            push_lifo(base, &mut state, 0, 5).ok();
            let last = guest::read_i64(base + 0x10).unwrap_or(-1);
            let count = guest::read_i64(base + 0x18).unwrap_or(-1);
            assert_eq!(count, round.min(17));
            let at = base + 0x20 + last as u64 * 0x30;
            assert_eq!(guest::read_u64(at).unwrap_or(1), 2 * round as u64);
            assert_eq!(sampling_of(at), round);
        }
        assert_eq!(guest::read_i64(base + 8).unwrap_or(0), 17);
    }

    #[test]
    fn fill_writes_the_style_set_and_the_player_one_lifo() {
        let (base, _guard) = guest_memory();
        let mut input = Input::new(&Config::default());
        input.shm = Some(SharedMemory {
            handle: 1,
            addr: base,
        });
        assert!(fill(&mut input).is_ok());
        let entry = base + at::NPAD;
        assert_eq!(guest::read_u32(entry).unwrap_or(0), style::FULL_KEY);
        assert_eq!(guest::read_i64(entry + at::FULL_KEY + 0x18).unwrap_or(0), 1);
        assert_eq!(
            guest::read_i64(entry + at::SIX_FULL_KEY + 0x18).unwrap_or(0),
            1
        );
        let other = base + at::NPAD + at::NPAD_STRIDE;
        assert_eq!(guest::read_u32(other).unwrap_or(1), 0);
        assert!(fill(&mut input).is_ok());
        assert_eq!(guest::read_i64(entry + at::FULL_KEY + 0x18).unwrap_or(0), 2);
    }

    #[test]
    fn fill_without_memory_does_nothing() {
        let (_base, _guard) = guest_memory();
        let mut input = Input::new(&Config::default());
        assert!(fill(&mut input).is_ok());
    }
}
