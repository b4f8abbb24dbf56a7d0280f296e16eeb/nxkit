//! Small helpers for the arguments of the recording functions.

use nxkit_abi::guest;
use nxkit_nvn::ext::MAX_EXT_WORDS;

use crate::nvn::abi::{AbiResult, Obj, other};

/// The low 32 bits of an address.
pub(super) fn lo(a: u64) -> u32 {
    a as u32
}

/// The high 32 bits of an address.
pub(super) fn hi(a: u64) -> u32 {
    (a >> 32) as u32
}

/// The `NVNshaderStage` of a function that takes a stage. The two extra stages of NVN2 share the slots of the
/// tessellation stages. A stage that is still above 5 is an error.
pub(super) fn stage(st: i32) -> AbiResult<u16> {
    let s = match st {
        6 => 3,
        7 => 4,
        s => s,
    };
    u16::try_from(s)
        .ok()
        .filter(|s| *s < 6)
        .ok_or_else(|| other(format!("shader stage {st} is not valid")))
}

/// The bank of texture and image slots of a stage. A bank has 32 slots, so a slot is `bank * 32 + index`.
fn bank(stage: u16) -> u32 {
    match stage {
        0 => 0,
        1 => 1,
        5 => 2,
        _ => 3,
    }
}

/// The slot of a texture or image index. The index must be below 32.
pub(super) fn slot(stage: u16, index: i32) -> AbiResult<u8> {
    match u32::try_from(index) {
        Ok(i) if i < 32 => Ok((bank(stage) * 32 + i) as u8),
        _ => Err(other(format!("texture index {index} is not valid"))),
    }
}

/// Checks `first >= 0`, `n >= 0` and `first + n <= max`.
pub(super) fn span(first: i32, n: i32, max: u32) -> AbiResult<(u32, u32)> {
    match (u32::try_from(first), u32::try_from(n)) {
        (Ok(f), Ok(n)) if f + n <= max => Ok((f, n)),
        _ => Err(other(format!("first {first} and count {n} do not fit {max}"))),
    }
}

/// Reads `len` bytes from the guest into `out`, which holds `len.div_ceil(4)` words at least. The last word is padded
/// with zeros.
pub(super) fn load_bytes(addr: u64, len: usize, out: &mut [u32]) -> AbiResult<()> {
    guest::with_slice(addr, len as u64, |b| {
        out.fill(0);
        for (w, c) in out.iter_mut().zip(b.chunks(4)) {
            let mut a = [0u8; 4];
            a[..c.len()].copy_from_slice(c);
            *w = u32::from_le_bytes(a);
        }
    })?;
    Ok(())
}

/// The `i`-th `NVNbufferRange` of an array: the address and the size.
pub(super) fn range(addr: u64, i: u32) -> AbiResult<(u64, u64)> {
    let o = Obj(addr + u64::from(i) * 16);
    Ok((o.u64(0)?, o.u64(8)?))
}

/// The words of one extension record, built on the stack.
pub(super) struct Words {
    w: [u32; MAX_EXT_WORDS],
    n: usize,
}

impl Words {
    /// No words.
    pub(super) fn new() -> Self {
        Self {
            w: [0; MAX_EXT_WORDS],
            n: 0,
        }
    }

    /// Adds a word. A word past the end of the record is an error.
    pub(super) fn u32(&mut self, v: u32) -> AbiResult<&mut Self> {
        let slot = self
            .w
            .get_mut(self.n)
            .ok_or_else(|| other("a record has too many words"))?;
        *slot = v;
        self.n += 1;
        Ok(self)
    }

    /// Adds a 64-bit value as two words.
    pub(super) fn u64(&mut self, v: u64) -> AbiResult<&mut Self> {
        self.u32(lo(v))?.u32(hi(v))
    }

    /// Adds `len` bytes of guest memory, padded to whole words.
    pub(super) fn load(&mut self, addr: u64, len: usize) -> AbiResult<&mut Self> {
        let words = len.div_ceil(4);
        let out = self
            .w
            .get_mut(self.n..self.n + words)
            .ok_or_else(|| other("a record has too many words"))?;
        load_bytes(addr, len, out)?;
        self.n += words;
        Ok(self)
    }

    /// The words so far.
    pub(super) fn as_slice(&self) -> &[u32] {
        &self.w[..self.n]
    }
}
