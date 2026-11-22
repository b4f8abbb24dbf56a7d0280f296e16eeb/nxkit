//! The structs of the hook points. All are `repr(C)` plain data, and none holds a pointer, so a patch reads them
//! without `unsafe`.

/// The longest program key that a [`Key`] holds. A longer key is cut.
pub const KEY_LEN: usize = 64;

/// The most colour attachments of a pass.
pub const MAX_COLORS: usize = 8;

/// The attachments of a pass: the colour slots and the depth and stencil slot.
pub const MAX_ATTACH: usize = MAX_COLORS + 1;

/// The most different program pairs that a [`PassInfo`] lists.
pub const MAX_PROGRAMS: usize = 16;

/// The most texture binds of one draw that a [`DrawInfo`] lists.
pub const MAX_TEXTURES: usize = 16;

/// The most texture slots that one [`DrawAnswer`](crate::DrawAnswer) forces.
pub const MAX_FORCED: usize = 4;

/// The value of a texture id that the host does not know (the handle did not resolve in this frame).
pub const TEX_UNKNOWN: u32 = u32::MAX;

/// The slot number of the depth and stencil attachment in [`AttachInfo::slot`].
pub const SLOT_DEPTH: u32 = 8;

/// A program key or a format name: UTF-8, padded with zero bytes. The key of a shader is the name that the shader
/// step gives to its library.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    /// The bytes of the text, then zeros.
    pub bytes: [u8; KEY_LEN],
}

impl Default for Key {
    fn default() -> Self {
        Key::EMPTY
    }
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

impl Key {
    /// The empty key: no program.
    pub const EMPTY: Key = Key {
        bytes: [0; KEY_LEN],
    };

    /// A key of `text`. A text longer than [`KEY_LEN`] bytes is cut at a character boundary.
    pub fn new(text: &str) -> Key {
        let mut end = text.len().min(KEY_LEN);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let mut bytes = [0u8; KEY_LEN];
        bytes[..end].copy_from_slice(&text.as_bytes()[..end]);
        Key { bytes }
    }

    /// The text of the key. A key that is not UTF-8 reads as the empty text.
    pub fn as_str(&self) -> &str {
        let end = self.bytes.iter().position(|b| *b == 0).unwrap_or(KEY_LEN);
        std::str::from_utf8(&self.bytes[..end]).unwrap_or("")
    }

    /// True for the empty key.
    pub fn is_empty(&self) -> bool {
        self.bytes[0] == 0
    }
}

/// The blend state of one colour target. The numbers are the `NVNblendFunc` and `NVNblendEquation` values that the
/// game used.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlendState {
    /// 1 when blending is on.
    pub enable: u32,
    /// The source factor of RGB.
    pub src_rgb: u32,
    /// The destination factor of RGB.
    pub dst_rgb: u32,
    /// The equation of RGB.
    pub op_rgb: u32,
    /// The source factor of alpha.
    pub src_alpha: u32,
    /// The destination factor of alpha.
    pub dst_alpha: u32,
    /// The equation of alpha.
    pub op_alpha: u32,
}

/// What the host gives a patch at load: the log function. The host keeps the struct alive for as long as the patch
/// is loaded, but a patch should copy the function pointer.
#[repr(C)]
pub struct HostApi {
    /// [`crate::ABI_VERSION`] of the host.
    pub abi_version: u32,
    /// Prints `len` bytes of UTF-8 text at `text` into the host log, with the prefix `nxkit-patch:`. Any thread.
    pub log_raw: extern "C" fn(text: *const u8, len: usize),
}

impl HostApi {
    /// Prints a line into the host log.
    pub fn log(&self, line: &str) {
        (self.log_raw)(line.as_ptr(), line.len());
    }
}

/// A frame starts to be encoded.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameBegin {
    /// The number of the frame since the host started. It counts the frames that the encoder gets.
    pub frame: u64,
    /// The passes of the frame (all kinds).
    pub passes: u32,
    /// The draws of the frame.
    pub draws: u32,
}

/// A frame is encoded and committed. The GPU still runs.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameEnd {
    /// The number of the frame, as in [`FrameBegin`].
    pub frame: u64,
    /// The render passes that the encoder made.
    pub render_passes: u32,
    /// The draws that the encoder made.
    pub draws: u32,
    /// The draws and other commands that the encoder left out, for any reason.
    pub skipped: u32,
    /// The command buffers of the frame.
    pub command_buffers: u32,
    /// The pipelines that the frame built.
    pub pipeline_builds: u32,
    /// The time of the encoding on the CPU, in nanoseconds.
    pub encode_ns: u64,
}

/// One attachment of a render pass.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AttachInfo {
    /// The id of the texture, as in the draw's texture binds.
    pub tex_id: u32,
    /// The colour slot 0 to 7, or [`SLOT_DEPTH`].
    pub slot: u32,
    /// The width in pixels, after the render scale.
    pub width: u32,
    /// The height in pixels, after the render scale.
    pub height: u32,
    /// The load action: 0 load, 1 clear, 2 do not care.
    pub load: u32,
    /// The store action: 0 store, 1 do not care.
    pub store: u32,
    /// The pixel format, as text of the Metal format name.
    pub format: Key,
}

/// The programs of one draw: the vertex key and the fragment key (empty without one).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProgramKeys {
    /// The key of the vertex shader. Empty for a mesh draw.
    pub vertex: Key,
    /// The key of the fragment shader. Empty for a draw that writes depth only.
    pub fragment: Key,
}

/// A render pass, before it is encoded. Read only: the answer changes it.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PassInfo {
    /// The number of the frame.
    pub frame: u64,
    /// The index of the pass in the frame, as in the pass profile.
    pub index: u32,
    /// The ordinal of the first draw of the pass in the frame, as in [`DrawInfo::draw`].
    pub first_draw: u32,
    /// The number of draws of the pass.
    pub draw_count: u32,
    /// The number of valid entries in `attach`.
    pub attach_count: u32,
    /// The attachments, colours first.
    pub attach: [AttachInfo; MAX_ATTACH],
    /// The number of valid entries in `programs`.
    pub program_count: u32,
    /// The different program pairs of the draws, in the order of the first draw that uses them. At most
    /// [`MAX_PROGRAMS`]. `programs_total` says how many there are.
    pub programs: [ProgramKeys; MAX_PROGRAMS],
    /// The number of different program pairs, also those that did not fit in `programs`.
    pub programs_total: u32,
}

impl PassInfo {
    /// The valid attachments.
    pub fn attachments(&self) -> &[AttachInfo] {
        &self.attach[..(self.attach_count as usize).min(MAX_ATTACH)]
    }

    /// The valid program pairs.
    pub fn program_list(&self) -> &[ProgramKeys] {
        &self.programs[..(self.program_count as usize).min(MAX_PROGRAMS)]
    }
}

/// A texture bind of a draw.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TexBind {
    /// The stage: 0 vertex, 1 fragment, 2 compute, 3 task, 4 mesh.
    pub stage: u32,
    /// The slot of the shader's reflection.
    pub slot: u32,
    /// The id of the texture that the handle resolved to, or [`TEX_UNKNOWN`].
    pub tex_id: u32,
    /// The handle of the game: the texture id in the low 20 bits and the sampler id above.
    pub handle: u64,
}

/// A draw, before it is encoded. Read only: the answer changes it.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DrawInfo {
    /// The number of the frame.
    pub frame: u64,
    /// The index of the pass of the draw.
    pub pass: u32,
    /// The ordinal of the draw in the frame.
    pub draw: u32,
    /// `NVNdrawPrimitive`: 0 points, 1 lines, 3 line strip, 4 triangles, 5 triangle strip.
    pub topology: u32,
    /// The programs.
    pub programs: ProgramKeys,
    /// The number of valid entries in `textures`.
    pub texture_count: u32,
    /// The texture binds, at most [`MAX_TEXTURES`].
    pub textures: [TexBind; MAX_TEXTURES],
    /// The blend state of each colour target.
    pub blend: [BlendState; MAX_COLORS],
    /// The colour write mask of each target: bit 0 red to bit 3 alpha.
    pub write_masks: [u8; MAX_COLORS],
    /// 1 when the depth test is on.
    pub depth_test: u32,
    /// 1 when depth writes are on.
    pub depth_write: u32,
    /// 1 when the stencil test is on.
    pub stencil_test: u32,
    /// The stencil reference of the front and the back face.
    pub stencil_ref: [u32; 2],
}

impl DrawInfo {
    /// The valid texture binds.
    pub fn texture_list(&self) -> &[TexBind] {
        &self.textures[..(self.texture_count as usize).min(MAX_TEXTURES)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, size_of};

    /// The layout is the ABI. When a size here changes, change [`crate::ABI_VERSION`] and this table together.
    #[test]
    fn layout_is_the_abi() {
        assert_eq!(crate::ABI_VERSION, 1);
        assert_eq!((size_of::<Key>(), align_of::<Key>()), (64, 1));
        assert_eq!((size_of::<BlendState>(), align_of::<BlendState>()), (28, 4));
        assert_eq!(
            (size_of::<AttachInfo>(), align_of::<AttachInfo>()),
            (24 + 64, 4)
        );
        assert_eq!(
            (size_of::<ProgramKeys>(), align_of::<ProgramKeys>()),
            (128, 1)
        );
        assert_eq!((size_of::<TexBind>(), align_of::<TexBind>()), (24, 8));
        assert_eq!((size_of::<FrameBegin>(), align_of::<FrameBegin>()), (16, 8));
        assert_eq!((size_of::<FrameEnd>(), align_of::<FrameEnd>()), (40, 8));
        assert_eq!(
            (size_of::<PassInfo>(), align_of::<PassInfo>()),
            (8 + 4 * 4 + 9 * 88 + 4 + 16 * 128 + 4, 8)
        );
        assert_eq!(size_of::<DrawInfo>() % 8, 0);
        assert_eq!(size_of::<HostApi>(), 16);
    }

    #[test]
    fn key_cuts_at_a_character_boundary() {
        let long = "é".repeat(KEY_LEN);
        let key = Key::new(&long);
        assert_eq!(key.as_str().len(), KEY_LEN);
        assert!(key.as_str().chars().all(|c| c == 'é'));
        assert_eq!(Key::new("abc").as_str(), "abc");
        assert!(Key::EMPTY.is_empty());
    }
}
