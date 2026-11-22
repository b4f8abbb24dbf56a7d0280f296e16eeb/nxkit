//! The table of hooks that a patch fills at load.

use crate::answer::{DrawAnswer, PassAnswer};
use crate::types::{DrawInfo, FrameBegin, FrameEnd, PassInfo};

/// A patch has a frame hook.
pub const HOOK_FRAME: u32 = 1;
/// A patch has a pass hook.
pub const HOOK_PASS: u32 = 2;
/// A patch has a draw hook.
pub const HOOK_DRAW: u32 = 4;

/// The hook before a frame is encoded.
pub type FrameBeginFn = extern "C" fn(&FrameBegin);
/// The hook after a frame is encoded and committed.
pub type FrameEndFn = extern "C" fn(&FrameEnd);
/// The hook before a render pass is encoded.
pub type PassFn = extern "C" fn(&PassInfo, &mut PassAnswer);
/// The hook before a draw is encoded.
pub type DrawFn = extern "C" fn(&DrawInfo, &mut DrawAnswer);

/// The longest patch name, in bytes, with the closing zero.
pub const NAME_LEN: usize = 32;

/// What a patch gives the host: its name and its hooks. A hook that is `None` is not called.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PatchTable {
    /// [`crate::ABI_VERSION`] as the patch was built. The macro fills it.
    pub abi_version: u32,
    /// The name of the patch, UTF-8, padded with zeros. A second load with the same name replaces the first.
    pub name: [u8; NAME_LEN],
    /// Before a frame is encoded.
    pub frame_begin: Option<FrameBeginFn>,
    /// After a frame is encoded.
    pub frame_end: Option<FrameEndFn>,
    /// Before each render pass.
    pub pass: Option<PassFn>,
    /// Before each draw.
    pub draw: Option<DrawFn>,
}

impl Default for PatchTable {
    fn default() -> Self {
        PatchTable {
            abi_version: crate::ABI_VERSION,
            name: [0; NAME_LEN],
            frame_begin: None,
            frame_end: None,
            pass: None,
            draw: None,
        }
    }
}

impl PatchTable {
    /// Sets the name. A longer name is cut.
    pub fn set_name(&mut self, name: &str) {
        let mut end = name.len().min(NAME_LEN - 1);
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        self.name = [0; NAME_LEN];
        self.name[..end].copy_from_slice(&name.as_bytes()[..end]);
    }

    /// The name of the patch. A name that is not UTF-8 reads as the empty text.
    pub fn name(&self) -> &str {
        let end = self.name.iter().position(|b| *b == 0).unwrap_or(NAME_LEN);
        std::str::from_utf8(&self.name[..end]).unwrap_or("")
    }

    /// The hooks that are set, as [`HOOK_FRAME`], `HOOK_PASS` and `HOOK_DRAW` bits.
    pub fn hook_mask(&self) -> u32 {
        use self::{HOOK_DRAW, HOOK_FRAME, HOOK_PASS};
        let bit = |on: bool, b: u32| if on { b } else { 0 };
        bit(
            self.frame_begin.is_some() || self.frame_end.is_some(),
            HOOK_FRAME,
        ) | bit(self.pass.is_some(), HOOK_PASS)
            | bit(self.draw.is_some(), HOOK_DRAW)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_layout_is_the_abi() {
        assert_eq!(std::mem::size_of::<PatchTable>(), 4 + 32 + 4 + 4 * 8);
    }

    #[test]
    fn name_round_trips() {
        let mut t = PatchTable::default();
        t.set_name("skip-fs");
        assert_eq!(t.name(), "skip-fs");
        t.set_name(&"x".repeat(100));
        assert_eq!(t.name().len(), NAME_LEN - 1);
    }
}
