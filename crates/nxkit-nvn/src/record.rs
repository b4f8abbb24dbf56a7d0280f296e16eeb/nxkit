//! The record format in the command memory of the game.
//!
//! The recorder writes one record per NVN command into the memory that the game gave with
//! `nvnCommandBufferAddCommandMemory`. A record is a multiple of 8 bytes and starts with an 8-byte header.
//! The header has the operation (`u16`), a small immediate value (`u16`) and the size of the record in bytes (`u32`).
//! All values are little endian. Addresses in a record are GPU addresses, which are the Metal addresses (refer to `space`).
//! The value 0 means "no texture". Most records need only the header.
//!
//! The record sizes are small on purpose. A record must never take more bytes than the real NVN command memory takes for the
//! same command, because games size their command memory with `nvnCommandBufferGetCommandMemoryUsed`.
//! The largest of the first records is `SetTargets` with 88 bytes. A draw takes 8 bytes.
//! A command that carries parameters is an [`Ext`] record (refer to [`crate::ext`]). The largest takes 120 bytes. The
//! real NVN sizes of these commands are not checked (R17), so a game may see a larger
//! `nvnCommandBufferGetCommandMemoryUsed` than on the console.

use crate::error::{NvnError, Result};
use crate::ext::{Ext, ExtCode, FIRST_EXT_OP};
use crate::frame::{DrawState, MAX_COLOR};
use crate::mem::GuestMem;

/// The size of the header of a record.
pub const HEADER_BYTES: u32 = 8;
/// The room that the recorder keeps after each record, for a `Jump` to the next memory block.
pub const JUMP_BYTES: u32 = 16;
/// The largest record in bytes.
pub const MAX_RECORD_BYTES: usize = 120;
/// The value of a texture handle that unbinds the slot.
pub const NO_HANDLE: u64 = u64::MAX;

/// One command in the command memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Record {
    /// The end of a recording. It is the last record of a handle.
    End,
    /// The recording goes on at this host address.
    Jump(u64),
    /// Plays another recording. The value is its handle.
    Call(u64),
    /// The attachments, by the GPU address of their texture. 0 means no attachment.
    SetTargets {
        /// The colour targets by slot.
        color: [u64; MAX_COLOR],
        /// The depth and stencil target.
        depth: u64,
    },
    /// Tells whether the scissor covers the whole target.
    SetScissor {
        /// The scissor covers the whole target.
        full: bool,
    },
    /// A clear of one colour slot.
    ClearColor {
        /// The colour slot.
        slot: u8,
    },
    /// A clear of depth and stencil.
    ClearDepthStencil,
    /// Tells that contents are not needed.
    Discard {
        /// Bit n is set for colour slot n.
        color: u8,
        /// Depth and stencil.
        depth: bool,
    },
    /// The draw state that the next draws use.
    BindState(DrawState),
    /// A texture handle for a sampling slot. [`NO_HANDLE`] unbinds.
    BindTexture {
        /// The slot.
        slot: u8,
        /// The handle.
        handle: u64,
    },
    /// Texture handles that the GPU reads when the command runs (`nvnCommandBufferBindTexturesDeferred`). The game
    /// records the command once and writes the handles at `src` before each submit, so a reader takes them from guest
    /// memory when it reads the record ([`deferred_handle`]).
    BindTexturesDeferred {
        /// The slot of the first handle.
        first: u8,
        /// The number of handles.
        count: u8,
        /// The host address of the first handle. The handles are 8 bytes apart.
        src: u64,
        /// The texture descriptor base of the command buffer at recording.
        tex_base: u32,
        /// The sampler descriptor base of the command buffer at recording.
        samp_base: u32,
    },
    /// A texture handle for an image slot. [`NO_HANDLE`] unbinds.
    BindImage {
        /// The slot.
        slot: u8,
        /// The handle.
        handle: u64,
    },
    /// A draw with the current state.
    Draw,
    /// A compute dispatch.
    Dispatch,
    /// A copy. 0 means a buffer.
    Copy {
        /// The GPU address of the source texture.
        src: u64,
        /// The GPU address of the destination texture.
        dst: u64,
    },
    /// A resolve of a multisample texture.
    Downsample {
        /// The multisample texture.
        src: u64,
        /// The single sample texture.
        dst: u64,
    },
    /// A barrier with its bits.
    Barrier(u32),
    /// A fence or a wait on the queue.
    Sync,
    /// A present of a texture.
    Present(u64),
    /// A CPU readback of a texture.
    CpuRead(u64),
    /// A command with parameters (refer to [`crate::ext`]).
    Ext(Ext),
}

const OP_END: u16 = 0;
const OP_JUMP: u16 = 1;
const OP_CALL: u16 = 2;
const OP_TARGETS: u16 = 3;
const OP_SCISSOR: u16 = 4;
const OP_CLEAR_COLOR: u16 = 5;
const OP_CLEAR_DEPTH: u16 = 6;
const OP_DISCARD: u16 = 7;
const OP_STATE: u16 = 8;
const OP_TEXTURE: u16 = 9;
const OP_IMAGE: u16 = 10;
const OP_DRAW: u16 = 11;
const OP_DISPATCH: u16 = 12;
const OP_COPY: u16 = 13;
const OP_DOWNSAMPLE: u16 = 14;
const OP_BARRIER: u16 = 15;
const OP_SYNC: u16 = 16;
const OP_PRESENT: u16 = 17;
const OP_CPU_READ: u16 = 18;
const OP_TEXTURES_DEFERRED: u16 = 19;

/// A record in its bytes.
#[derive(Clone, Copy, Debug)]
pub struct Encoded {
    bytes: [u8; MAX_RECORD_BYTES],
    len: usize,
}

impl Encoded {
    /// The bytes of the record.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    /// The size of the record in bytes.
    pub fn len(&self) -> u32 {
        self.len as u32
    }

    /// A record is never empty. This method exists for the usual pair with `len`.
    pub fn is_empty(&self) -> bool {
        false
    }
}

fn state_arg(s: &DrawState) -> u16 {
    let flags = u16::from(s.blend)
        | u16::from(s.depth_test) << 1
        | u16::from(s.depth_write) << 2
        | u16::from(s.stencil_test) << 3;
    u16::from(s.color_write) | flags << 8
}

fn state_of(arg: u16) -> DrawState {
    let flags = arg >> 8;
    DrawState {
        color_write: arg as u8,
        blend: flags & 1 != 0,
        depth_test: flags & 2 != 0,
        depth_write: flags & 4 != 0,
        stencil_test: flags & 8 != 0,
    }
}

impl Record {
    /// Encodes the record. A value that does not fit gives an error.
    pub fn encode(&self) -> Result<Encoded> {
        let mut e = Encoded {
            bytes: [0; MAX_RECORD_BYTES],
            len: 0,
        };
        let mut words = [0u64; 10];
        let mut n = 0;
        let (op, arg) = match *self {
            Self::End => (OP_END, 0),
            Self::Jump(a) => {
                words[0] = a;
                n = 1;
                (OP_JUMP, 0)
            }
            Self::Call(h) => {
                words[0] = h;
                n = 1;
                (OP_CALL, 0)
            }
            Self::SetTargets { color, depth } => {
                let mut mask = 0u64;
                for (i, a) in color.iter().enumerate() {
                    if *a != 0 {
                        mask |= 1 << i;
                        words[1 + n] = *a;
                        n += 1;
                    }
                }
                if depth != 0 {
                    mask |= 1 << MAX_COLOR;
                    words[1 + n] = depth;
                    n += 1;
                }
                words[0] = mask;
                n += 1;
                (OP_TARGETS, 0)
            }
            Self::SetScissor { full } => (OP_SCISSOR, u16::from(full)),
            Self::ClearColor { slot } => (OP_CLEAR_COLOR, u16::from(slot)),
            Self::ClearDepthStencil => (OP_CLEAR_DEPTH, 0),
            Self::Discard { color, depth } => {
                (OP_DISCARD, u16::from(color) | u16::from(depth) << 8)
            }
            Self::BindState(s) => (OP_STATE, state_arg(&s)),
            Self::BindTexture { slot, handle } => {
                words[0] = handle;
                n = 1;
                (OP_TEXTURE, u16::from(slot))
            }
            Self::BindImage { slot, handle } => {
                words[0] = handle;
                n = 1;
                (OP_IMAGE, u16::from(slot))
            }
            Self::BindTexturesDeferred { first, count, src, tex_base, samp_base } => {
                words[0] = src;
                words[1] = u64::from(tex_base) | u64::from(samp_base) << 32;
                n = 2;
                (OP_TEXTURES_DEFERRED, u16::from(first) | u16::from(count) << 8)
            }
            Self::Draw => (OP_DRAW, 0),
            Self::Dispatch => (OP_DISPATCH, 0),
            Self::Copy { src, dst } => {
                words[..2].copy_from_slice(&[src, dst]);
                n = 2;
                (OP_COPY, 0)
            }
            Self::Downsample { src, dst } => {
                words[..2].copy_from_slice(&[src, dst]);
                n = 2;
                (OP_DOWNSAMPLE, 0)
            }
            Self::Barrier(bits) => {
                words[0] = u64::from(bits);
                n = 1;
                (OP_BARRIER, 0)
            }
            Self::Sync => (OP_SYNC, 0),
            Self::Present(t) => {
                words[0] = t;
                n = 1;
                (OP_PRESENT, 0)
            }
            Self::CpuRead(t) => {
                words[0] = t;
                n = 1;
                (OP_CPU_READ, 0)
            }
            Self::Ext(x) => {
                e.len = x.encode_into(&mut e.bytes)?;
                return Ok(e);
            }
        };
        let len = HEADER_BYTES as usize + 8 * n;
        e.bytes[0..2].copy_from_slice(&op.to_le_bytes());
        e.bytes[2..4].copy_from_slice(&arg.to_le_bytes());
        e.bytes[4..8].copy_from_slice(&(len as u32).to_le_bytes());
        for (i, w) in words[..n].iter().enumerate() {
            e.bytes[8 + 8 * i..16 + 8 * i].copy_from_slice(&w.to_le_bytes());
        }
        e.len = len;
        match self {
            Self::BindTexture { slot, .. } | Self::BindImage { slot, .. } if *slot >= 128 => {
                Err(NvnError::FieldRange("binding slot"))
            }
            Self::BindTexturesDeferred { first, count, .. }
                if usize::from(*first) + usize::from(*count) > 128 =>
            {
                Err(NvnError::FieldRange("binding slot"))
            }
            Self::ClearColor { slot } if *slot as usize >= MAX_COLOR => {
                Err(NvnError::FieldRange("colour slot"))
            }
            Self::Barrier(bits) if *bits > 0xff => Err(NvnError::FieldRange("barrier bits")),
            _ => Ok(e),
        }
    }

    /// Reads the record at `host` and returns it with its size.
    pub fn read(mem: &impl GuestMem, host: u64) -> Result<(Self, u32)> {
        let mut buf = [0u8; MAX_RECORD_BYTES];
        // One read takes the header and the body. It fails when the record ends close to the end of the memory.
        let whole = mem.read(host, &mut buf).is_ok();
        let mut h = [0u8; 8];
        if whole {
            h.copy_from_slice(&buf[..8]);
        } else {
            mem.read(host, &mut h)?;
        }
        let op = u16::from_le_bytes([h[0], h[1]]);
        let arg = u16::from_le_bytes([h[2], h[3]]);
        let size = u32::from_le_bytes([h[4], h[5], h[6], h[7]]);
        if size < HEADER_BYTES || size % 8 != 0 || size as usize > MAX_RECORD_BYTES {
            return Err(NvnError::BadRecord("size"));
        }
        if !whole {
            mem.read(host + u64::from(HEADER_BYTES), &mut buf[8..size as usize])?;
        }
        let body = &buf[HEADER_BYTES as usize..size as usize];
        let w = |i: usize| -> Result<u64> {
            body.get(8 * i..8 * i + 8)
                .map(|b| u64::from_le_bytes(b.try_into().unwrap_or([0; 8])))
                .ok_or(NvnError::BadRecord("payload is too short"))
        };
        let rec = match op {
            OP_END => Self::End,
            OP_JUMP => Self::Jump(w(0)?),
            OP_CALL => Self::Call(w(0)?),
            OP_TARGETS => {
                let mask = w(0)?;
                let (mut color, mut depth, mut k) = ([0u64; MAX_COLOR], 0, 1);
                for (i, c) in color.iter_mut().enumerate() {
                    if mask & (1 << i) != 0 {
                        *c = w(k)?;
                        k += 1;
                    }
                }
                if mask & (1 << MAX_COLOR) != 0 {
                    depth = w(k)?;
                }
                Self::SetTargets { color, depth }
            }
            OP_SCISSOR => Self::SetScissor { full: arg != 0 },
            OP_CLEAR_COLOR if (arg as usize) < MAX_COLOR => Self::ClearColor { slot: arg as u8 },
            OP_CLEAR_DEPTH => Self::ClearDepthStencil,
            OP_DISCARD => Self::Discard {
                color: arg as u8,
                depth: arg >> 8 != 0,
            },
            OP_STATE => Self::BindState(state_of(arg)),
            OP_TEXTURE if arg < 128 => Self::BindTexture {
                slot: arg as u8,
                handle: w(0)?,
            },
            OP_IMAGE if arg < 128 => Self::BindImage {
                slot: arg as u8,
                handle: w(0)?,
            },
            OP_TEXTURES_DEFERRED => {
                let bases = w(1)?;
                Self::BindTexturesDeferred {
                    first: arg as u8,
                    count: (arg >> 8) as u8,
                    src: w(0)?,
                    tex_base: bases as u32,
                    samp_base: (bases >> 32) as u32,
                }
            }
            OP_DRAW => Self::Draw,
            OP_DISPATCH => Self::Dispatch,
            OP_COPY => Self::Copy {
                src: w(0)?,
                dst: w(1)?,
            },
            OP_DOWNSAMPLE => Self::Downsample {
                src: w(0)?,
                dst: w(1)?,
            },
            OP_BARRIER => Self::Barrier(w(0)? as u32),
            OP_SYNC => Self::Sync,
            OP_PRESENT => Self::Present(w(0)?),
            OP_CPU_READ => Self::CpuRead(w(0)?),
            op if op >= FIRST_EXT_OP => {
                let code = ExtCode::from_u16(op).ok_or(NvnError::BadRecord("unknown operation"))?;
                Self::Ext(Ext::from_payload(code, arg, body))
            }
            _ => return Err(NvnError::BadRecord("unknown operation")),
        };
        Ok((rec, size))
    }
}

/// Moves the ids of a handle into the descriptor table of a command buffer. The texture id is bits 0 to 19, the
/// sampler id bits 20 to 31. An id of 0 means no descriptor and stays 0. `tex` and `samp` say which ids move.
pub fn adjust_handle(h: u64, tex_base: u32, samp_base: u32, tex: bool, samp: bool) -> u64 {
    let h = h as u32;
    let (t, s) = (h & 0xf_ffff, h >> 20);
    let t = if tex && t != 0 { t + tex_base } else { t };
    let s = if samp && s != 0 { s + samp_base } else { s };
    u64::from(s << 20 | t & 0xf_ffff)
}

/// The handle `k` of a [`Record::BindTexturesDeferred`] as guest memory holds it now: the raw value at its address
/// and the handle with the descriptor bases applied.
pub fn deferred_handle(
    mem: &impl GuestMem,
    src: u64,
    k: u8,
    tex_base: u32,
    samp_base: u32,
) -> Result<(u64, u64, u64)> {
    let at = src + 8 * u64::from(k);
    let mut b = [0u8; 8];
    mem.read(at, &mut b)?;
    let raw = u64::from_le_bytes(b);
    Ok((at, raw, adjust_handle(raw, tex_base, samp_base, true, true)))
}

#[cfg(test)]
mod tests {
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
        assert_eq!(deferred_handle(&m, at, 1, 100, 10).unwrap().2, 109 | 12 << 20);
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
        let too_many = (0..=crate::ext::MAX_EXT_WORDS)
            .fold(Ext::new(ExtCode::Push, 0), |e, i| e.u32(i as u32));
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
}
