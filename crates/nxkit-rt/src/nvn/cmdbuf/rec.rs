//! How a command buffer function puts a record into command memory.
//!
//! The write position of a command buffer is kept in the guest `NVNcommandBuffer` itself, in bytes that the host owns
//! (the offsets below). The common case is a record that fits into the current block. It reads the position and the
//! limit from the object, writes the record straight into command memory and stores the new position. It takes no
//! lock, finds no registry entry, builds no `Vec` and writes a record once (the writer puts the words of the record
//! into command memory and nowhere else).
//!
//! The rest is the slow path. It runs when a block is full, at `AddCommandMemory`, `BeginRecording`,
//! `EndRecording`, at the getters and for `CallCommands`. It finds the [`Recorder`] of the command buffer in a
//! registry, folds the records that the fast path wrote into the [`CommandBuffer`] of nxkit-nvn (`commit`), does its
//! work and writes the new position and limit back. The memory callback of the game runs on this path and with no lock
//! held, so it may call `AddCommandMemory` itself.

use std::sync::OnceLock;

use nxkit_abi::guest;
use nxkit_nvn::cmdbuf::{CommandBuffer, Recording};
use nxkit_nvn::error::NvnError;
use nxkit_nvn::ext::{ExtCode, MAX_EXT_WORDS, ext_size, write_ext};
use nxkit_nvn::record::{MAX_RECORD_BYTES, Record};

use crate::nvn::abi::{AbiResult, Obj, Registry, fatal, guard, magic, other};
use crate::nvn::ctx::{GuestMemory, gpu};

/// The magic of an initialised command buffer.
pub(super) const MAGIC: u32 = magic(b'C', b'B', b'U', b'F');
/// The magic.
pub(super) const O_MAGIC: u64 = 0x00;
/// Nonzero between `BeginRecording` and `EndRecording`.
pub(super) const O_REC: u64 = 0x04;
/// The host address of the next record.
pub(super) const O_POS: u64 = 0x08;
/// A record fits when `pos + size <= limit`. The bytes from 0x10 to 0x17 belong to the game.
pub(super) const O_LIMIT: u64 = 0x18;
/// The id base of the texture pool of the last `SetTexturePool`.
pub(super) const O_TEX_BASE: u64 = 0x30;
/// The id base of the sampler pool of the last `SetSamplerPool`.
pub(super) const O_SAMP_BASE: u64 = 0x34;
/// The copy row stride.
pub(super) const O_ROW_STRIDE: u64 = 0x38;
/// The copy image stride.
pub(super) const O_IMAGE_STRIDE: u64 = 0x40;
/// The bytes that the fast path reads in one range check.
const HEAD_BYTES: u64 = 0x20;

/// How often a record that does not fit calls the memory callback before the game stops.
const MAX_CALLBACKS: u32 = 2;
/// `NVN_COMMAND_BUFFER_MEMORY_EVENT_OUT_OF_COMMAND_MEMORY`.
const EVENT_OUT_OF_COMMAND_MEMORY: i32 = 0;

/// The memory callback of a command buffer.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Hook {
    pub(super) func: u64,
    pub(super) data: u64,
    pub(super) enabled: bool,
}

/// What the host keeps of a command buffer besides the bytes in the guest object.
#[derive(Debug, Default)]
pub(super) struct Recorder {
    pub(super) buf: CommandBuffer,
    pub(super) hook: Hook,
    pub(super) control: u64,
}

fn recorders() -> &'static Registry<Recorder> {
    static R: OnceLock<Registry<Recorder>> = OnceLock::new();
    R.get_or_init(Registry::new)
}

/// Makes a new recorder for the command buffer at `cb`. A recorder that was there is dropped.
pub(super) fn create(cb: u64) {
    recorders().insert(cb, Recorder::default());
}

/// Drops the recorder of `cb`.
pub(super) fn destroy(cb: u64) {
    recorders().remove(cb);
}

/// Runs `f` on the recorder of `cb`. The closure must not call back into the registry.
pub(super) fn with_recorder<R>(
    cb: u64,
    f: impl FnOnce(&mut Recorder) -> AbiResult<R>,
) -> AbiResult<R> {
    recorders()
        .with(cb, f)
        .unwrap_or_else(|| Err(other("the command buffer is not initialised")))
}

/// Takes the records that the fast path wrote.
pub(super) fn sync_in(cb: u64, r: &mut Recorder) -> AbiResult<()> {
    r.buf.commit(Obj(cb).u64(O_POS)?);
    Ok(())
}

/// Writes the position and the limit of the command buffer into the guest object.
pub(super) fn sync_out(cb: u64, r: &Recorder) -> AbiResult<()> {
    let c = r.buf.cursor();
    Obj(cb).set_u64(O_POS, c.pos)?;
    Obj(cb).set_u64(O_LIMIT, c.limit)
}

fn le32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn le64(b: &[u8], off: usize) -> u64 {
    u64::from(le32(b, off)) | (u64::from(le32(b, off + 4)) << 32)
}

/// The position where a record of `size` bytes goes, or None when the fast path does not apply.
fn room(cb: u64, size: u64) -> Option<u64> {
    guest::with_slice(cb, HEAD_BYTES, |b| {
        let (rec, pos, limit) = (le32(b, 4), le64(b, 8), le64(b, 0x18));
        (rec != 0 && pos.saturating_add(size) <= limit).then_some(pos)
    })
    .ok()
    .flatten()
}

/// Records an extension record. At most [`MAX_EXT_WORDS`] words.
pub(super) fn emit(name: &'static str, cb: u64, code: ExtCode, arg: u16, words: &[u32]) {
    if words.len() > MAX_EXT_WORDS {
        fatal(name, "a record has too many words");
    }
    let size = ext_size(words.len());
    if let Some(pos) = room(cb, size as u64) {
        let wrote = guest::with_slice_mut(pos, size as u64, |b| write_ext(b, code, arg, words));
        if wrote.is_ok() && Obj(cb).set_u64(O_POS, pos + size as u64).is_ok() {
            return;
        }
    }
    let mut buf = [0u8; MAX_RECORD_BYTES];
    write_ext(&mut buf[..size], code, arg, words);
    slow(name, cb, &buf[..size]);
}

/// Records one of the first records of nxkit-nvn.
pub(super) fn emit_rec(name: &'static str, cb: u64, rec: Record) {
    let e = rec.encode().unwrap_or_else(|err| fatal(name, err));
    let bytes = e.as_bytes();
    let size = bytes.len() as u64;
    if let Some(pos) = room(cb, size) {
        let wrote = guest::with_slice_mut(pos, size, |b| b.copy_from_slice(bytes));
        if wrote.is_ok() && Obj(cb).set_u64(O_POS, pos + size).is_ok() {
            return;
        }
    }
    slow(name, cb, bytes);
}

/// What one try of a slow write did.
enum Try<T> {
    Done(T),
    Need(u32, Hook),
}

fn slow(name: &'static str, cb: u64, bytes: &[u8]) {
    guard(name, || {
        retrying(cb, |r| {
            let res = r.buf.write_encoded(&mut GuestMemory, bytes, false);
            Ok(match res {
                Ok(()) => Try::Done(()),
                Err(NvnError::OutOfCommandMemory { need }) => Try::Need(need, r.hook),
                Err(e) => return Err(e.into()),
            })
        })
    });
}

/// Runs `step` on the recorder. When it asks for memory, calls the memory callback of the game (with no lock held)
/// and runs it again, up to [`MAX_CALLBACKS`] times.
fn retrying<T>(
    cb: u64,
    mut step: impl FnMut(&mut Recorder) -> AbiResult<Try<T>>,
) -> AbiResult<T> {
    for attempt in 0..=MAX_CALLBACKS {
        let t = with_recorder(cb, |r| {
            sync_in(cb, r)?;
            let t = step(r);
            sync_out(cb, r)?;
            t
        })?;
        match t {
            Try::Done(v) => return Ok(v),
            Try::Need(need, hook) => {
                if attempt == MAX_CALLBACKS {
                    return Err(other(format!(
                        "out of command memory ({need} bytes needed) after {MAX_CALLBACKS} callbacks"
                    )));
                }
                run_hook(cb, need, hook)?;
            }
        }
    }
    Err(other("out of command memory"))
}

fn run_hook(cb: u64, need: u32, hook: Hook) -> AbiResult<()> {
    if !hook.enabled || hook.func == 0 {
        return Err(other(format!(
            "out of command memory ({need} bytes needed) and no memory callback is set"
        )));
    }
    // SAFETY: a guest function pointer is a host function pointer (R18). NVN defines the callback as
    // `void (*)(NVNcommandBuffer *, NVNcommandBufferMemoryEvent, size_t, void *)`.
    let f = unsafe {
        std::mem::transmute::<usize, extern "C" fn(u64, i32, u64, u64)>(hook.func as usize)
    };
    f(cb, EVENT_OUT_OF_COMMAND_MEMORY, u64::from(need), hook.data);
    Ok(())
}

/// `EndRecording`. Gives the handle.
pub(super) fn end(name: &'static str, cb: u64) -> u64 {
    guard(name, || {
        let rec: Recording = retrying(cb, |r| match r.buf.end_recording(&mut GuestMemory) {
            Ok(rec) => {
                Obj(cb).set_u32(O_REC, 0)?;
                Ok(Try::Done(rec))
            }
            Err(NvnError::OutOfCommandMemory { need }) => Ok(Try::Need(need, r.hook)),
            Err(e) => Err(e.into()),
        })?;
        gpu(name).lock().recorded(&rec.spans);
        Ok(rec.handle.0)
    })
}
