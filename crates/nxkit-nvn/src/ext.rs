//! The extension record: every NVN command that carries parameters.
//!
//! The records in [`crate::record`] hold what the pass analysis needs. A game also records draws with their counts, viewports,
//! buffer bindings, copies with their regions and many scalar settings. Each of these is one [`Ext`] record. The header
//! is the one of every record (operation, argument, size). The operation is an [`ExtCode`], and the argument is
//! the sub-kind or a small index of that code. The payload is little endian `u32` words, padded with a zero word
//! to an even count, so the record size stays a multiple of 8.
//!
//! The decoder reads an [`Ext`] for its effect on the frame (a draw, a dispatch, a clear, a copy, a sync) and for the
//! draw state that a state bind sets. Everything else is state for the backend that replays the recording. The decoder skips it.
//!
//! A record that does not fit [`MAX_EXT_WORDS`] is an error at [`Record::encode`](crate::record::Record::encode).
//! Data that is longer than a record (push constants, inline uniform data, vertex states) goes into several records.

use crate::error::{NvnError, Result};
use crate::record::HEADER_BYTES;

/// The most payload words of one extension record.
pub const MAX_EXT_WORDS: usize = 28;

/// The first operation number of an extension record. The operations below belong to [`crate::record::Record`].
pub const FIRST_EXT_OP: u16 = 64;

/// What an extension record is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum ExtCode {
    /// A scalar setting. The argument is a [`Setting`].
    Setting = FIRST_EXT_OP,
    /// `BindProgram`. Words: program address (2), stage bits.
    Program,
    /// `BindVertexBuffer`. The argument is the stream. Words: address (2), size (2).
    VertexBuffer,
    /// `BindUniformBuffer`. The argument is `stage << 8 | index`. Words: address (2), size (2).
    UniformBuffer,
    /// `BindStorageBuffer`. The argument is `stage << 8 | index`. Words: address (2), size (2).
    StorageBuffer,
    /// `SetViewport`. The argument is the index. Words: x, y, width, height as `f32`.
    Viewport,
    /// `SetScissor`. The argument is the index. Words: x, y, width, height as `i32`.
    Scissor,
    /// `SetDepthRange`. The argument is the index. Words: near, far as `f32`.
    DepthRange,
    /// A state object bind. The low byte of the argument is a [`StateKind`], the high byte is the patch of the draw
    /// state (refer to [`StateKind`]). Words: the bytes of the state object.
    State,
    /// A chunk of vertex attribute (argument 0) or vertex stream (argument 1) states. Words: first entry, entry
    /// count of the chunk, the bytes of the entries.
    VertexState,
    /// `BindSeparateSampler`. The argument is `stage << 8 | index`. Words: the sampler id.
    SeparateSampler,
    /// The view of a render target. The argument is the slot, 8 means depth and stencil. Words: the 16 bytes of the view.
    TargetView,
    /// A chunk of push constants. The argument is `compute | length << 1`. Words: offset, the bytes.
    Push,
    /// A chunk of `UpdateUniformBuffer`. Words: buffer address (2), buffer size (2), offset in the buffer (2), the bytes.
    UpdateUniform,
    /// `SetTexturePool`. Words: pool address (2), id base.
    TexturePool,
    /// `SetSamplerPool`. Words: pool address (2), id base.
    SamplerPool,
    /// A draw. The argument is a [`DrawKind`]. Each kind has its own words.
    Draw,
    /// A compute dispatch. The argument 0 is direct with words x, y, z. The argument 1 is indirect with the buffer
    /// address (2).
    Dispatch,
    /// A clear of a colour slot. The argument is `slot | kind << 8` with kind 0 for `f32`, 1 for `i32` and 2 for `u32`
    /// values. Words: channel mask, r, g, b, a.
    ClearColor,
    /// `ClearDepthStencil`. Words: depth as `f32`, depth mask, stencil, stencil mask.
    ClearDepthStencil,
    /// `ClearBuffer`. Words: address (2), size (2), value.
    ClearBuffer,
    /// `ClearTexture`. The argument is `kind | has_view << 8`. Words: texture address (2), channel mask, r, g, b, a,
    /// view (4), region (6).
    ClearTexture,
    /// `CopyBufferToBuffer`. Words: source (2), destination (2), size (2), flags.
    CopyBufferToBuffer,
    /// `CopyBufferToTexture`. The argument is `has_view`. Words: texture address (2), buffer address (2), flags, view (4), region (6).
    CopyBufferToTexture,
    /// `CopyTextureToBuffer`. Same words as [`ExtCode::CopyBufferToTexture`].
    CopyTextureToBuffer,
    /// `CopyTextureToTexture`. The argument is `has_src_view | has_dst_view << 1`. Words: source address (2),
    /// destination address (2), flags, source view (4), source region (6), destination view (4), destination region (6).
    CopyTextureToTexture,
    /// `DownsampleTextureView`. The argument is `has_src_view | has_dst_view << 1`. Words: source address (2), destination address (2),
    /// source view (4), destination view (4).
    DownsampleView,
    /// `FenceSync`. Words: sync address (2), condition, flags.
    FenceSync,
    /// `SignalEvent`. Words: event address (2), mode, location, flags, value.
    SignalEvent,
    /// `WaitSync`. Words: sync address (2).
    WaitSync,
    /// `ReportValue`. Words: value, address (2), timestamp (2). The timestamp is the host clock at the recording.
    ReportValue,
    /// `ResetCounter`. Words: counter type.
    ResetCounter,
    /// `ReportCounter`. Words: counter type, address (2), timestamp (2). The timestamp is the host clock at the recording.
    ReportCounter,
    /// `BindTransformFeedbackBuffer`. The argument is the index. Words: address (2), size (2).
    TransformFeedbackBuffer,
    /// A transform feedback state change. The argument is 0 for begin, 1 for end, 2 for pause and 3 for resume.
    /// Words: control buffer address (2).
    TransformFeedback,
    /// `SetViewportSwizzles`, one viewport. The argument is the index. Words: the swizzles of x, y, z and w.
    ViewportSwizzle,
}

const CODES: [ExtCode; 36] = [
    ExtCode::Setting,
    ExtCode::Program,
    ExtCode::VertexBuffer,
    ExtCode::UniformBuffer,
    ExtCode::StorageBuffer,
    ExtCode::Viewport,
    ExtCode::Scissor,
    ExtCode::DepthRange,
    ExtCode::State,
    ExtCode::VertexState,
    ExtCode::SeparateSampler,
    ExtCode::TargetView,
    ExtCode::Push,
    ExtCode::UpdateUniform,
    ExtCode::TexturePool,
    ExtCode::SamplerPool,
    ExtCode::Draw,
    ExtCode::Dispatch,
    ExtCode::ClearColor,
    ExtCode::ClearDepthStencil,
    ExtCode::ClearBuffer,
    ExtCode::ClearTexture,
    ExtCode::CopyBufferToBuffer,
    ExtCode::CopyBufferToTexture,
    ExtCode::CopyTextureToBuffer,
    ExtCode::CopyTextureToTexture,
    ExtCode::DownsampleView,
    ExtCode::FenceSync,
    ExtCode::SignalEvent,
    ExtCode::WaitSync,
    ExtCode::ReportValue,
    ExtCode::ResetCounter,
    ExtCode::ReportCounter,
    ExtCode::TransformFeedbackBuffer,
    ExtCode::TransformFeedback,
    ExtCode::ViewportSwizzle,
];

impl ExtCode {
    /// The code of an operation number. None when the number is not an extension.
    pub fn from_u16(op: u16) -> Option<Self> {
        op.checked_sub(FIRST_EXT_OP)
            .and_then(|i| CODES.get(usize::from(i)))
            .copied()
    }

    /// Every code.
    pub fn all() -> &'static [ExtCode] {
        &CODES
    }
}

/// The scalar settings of [`ExtCode::Setting`]. The words of each are in the comment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum Setting {
    /// Width as `f32`.
    LineWidth,
    /// Size as `f32`.
    PointSize,
    /// Reference as `f32`.
    AlphaRef,
    /// Enable.
    DepthClamp,
    /// Enable, index.
    PrimitiveRestart,
    /// Enable.
    RasterizerDiscard,
    /// Mask.
    SampleMask,
    /// Action.
    TiledCacheAction,
    /// Width, height.
    TiledCacheTileSize,
    /// Enable.
    RenderEnable,
    /// Mode, address (2).
    RenderEnableConditional,
    /// r, g, b, a as `f32`.
    BlendColor,
    /// Face, mask.
    StencilValueMask,
    /// Face, mask.
    StencilMask,
    /// Face, reference.
    StencilRef,
    /// Factor, units, clamp as `f32`.
    PolygonOffsetClamp,
    /// Stride (2).
    CopyRowStride,
    /// Stride (2).
    CopyImageStride,
    /// Enable.
    ConservativeRaster,
}

/// The state objects that a bind carries ([`ExtCode::State`]).
///
/// The patch in the high byte of the argument gives the part of the draw state that the object decides. The analysis
/// reads it, so it never reads the object itself. Blend colour state, multisample and polygon states have no patch.
/// A colour state patches the blend flag (bit 0). A channel mask state patches the colour write mask (the whole byte).
/// A depth stencil state patches depth test (bit 0), depth write (bit 1) and stencil test (bit 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum StateKind {
    /// `NVNblendState`.
    Blend = 0,
    /// `NVNcolorState`.
    Color = 1,
    /// `NVNchannelMaskState`.
    ChannelMask = 2,
    /// `NVNmultisampleState`.
    Multisample = 3,
    /// `NVNpolygonState`.
    Polygon = 4,
    /// `NVNdepthStencilState`.
    DepthStencil = 5,
}

impl StateKind {
    /// The kind of a byte.
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::Blend,
            1 => Self::Color,
            2 => Self::ChannelMask,
            3 => Self::Multisample,
            4 => Self::Polygon,
            5 => Self::DepthStencil,
            _ => return None,
        })
    }
}

/// The draws of [`ExtCode::Draw`]. `mode` is the `NVNdrawPrimitive` and `type` the `NVNindexType`. A buffer address is two words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum DrawKind {
    /// mode, first, count.
    Arrays,
    /// mode, first, count, base instance, instance count.
    ArraysInstanced,
    /// mode, indirect buffer (2).
    ArraysIndirect,
    /// mode, type, count, index buffer (2), base vertex, base instance, instance count. Plain and base vertex
    /// draws are this kind with the defaults 0, 0 and 1.
    Elements,
    /// mode, type, index buffer (2), indirect buffer (2).
    ElementsIndirect,
    /// mode, indirect buffer (2), count buffer (2), maximum, stride (2).
    MultiArraysIndirectCount,
    /// mode, type, index buffer (2), indirect buffer (2), count buffer (2), maximum, stride (2).
    MultiElementsIndirectCount,
    /// first task, task count.
    MeshTasks,
    /// indirect buffer (2).
    MeshTasksIndirect,
    /// mode, transform feedback control buffer (2).
    TransformFeedback,
    /// `DrawTexture`: texture handle, destination region (4 `f32`), source region (4 `f32`).
    Texture,
}

/// The size in bytes of an extension record with `words` payload words. A multiple of 8.
pub const fn ext_size(words: usize) -> usize {
    8 + words.div_ceil(2) * 8
}

/// Writes an extension record into `out`, which is [`ext_size`] bytes long. The recorder of the C ABI uses this to
/// write straight into command memory, without building an [`Ext`].
pub fn write_ext(out: &mut [u8], op: ExtCode, arg: u16, words: &[u32]) {
    let size = ext_size(words.len());
    out[0..2].copy_from_slice(&(op as u16).to_le_bytes());
    out[2..4].copy_from_slice(&arg.to_le_bytes());
    out[4..8].copy_from_slice(&(size as u32).to_le_bytes());
    let body = &mut out[8..size];
    body.fill(0);
    for (w, o) in words.iter().zip(body.chunks_mut(4)) {
        o.copy_from_slice(&w.to_le_bytes());
    }
}

/// An extension record: a code, an argument and up to [`MAX_EXT_WORDS`] words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ext {
    /// What the record is.
    pub code: ExtCode,
    /// The sub-kind or index. The meaning depends on the code.
    pub arg: u16,
    len: u8,
    over: bool,
    words: [u32; MAX_EXT_WORDS],
}

impl Ext {
    /// An extension record without words.
    pub fn new(code: ExtCode, arg: u16) -> Self {
        Self {
            code,
            arg,
            len: 0,
            over: false,
            words: [0; MAX_EXT_WORDS],
        }
    }

    /// Adds a word. A word past [`MAX_EXT_WORDS`] makes the record fail at encoding.
    #[must_use]
    pub fn u32(mut self, v: u32) -> Self {
        match self.words.get_mut(usize::from(self.len)) {
            Some(w) => {
                *w = v;
                self.len += 1;
            }
            None => self.over = true,
        }
        self
    }

    /// Adds a signed word.
    #[must_use]
    pub fn i32(self, v: i32) -> Self {
        self.u32(v as u32)
    }

    /// Adds a float as its bits.
    #[must_use]
    pub fn f32(self, v: f32) -> Self {
        self.u32(v.to_bits())
    }

    /// Adds a 64-bit value as two words, low first.
    #[must_use]
    pub fn u64(self, v: u64) -> Self {
        self.u32(v as u32).u32((v >> 32) as u32)
    }

    /// Adds bytes. The last word is padded with zeros.
    #[must_use]
    pub fn bytes(mut self, b: &[u8]) -> Self {
        for c in b.chunks(4) {
            let mut w = [0u8; 4];
            w[..c.len()].copy_from_slice(c);
            self = self.u32(u32::from_le_bytes(w));
        }
        self
    }

    /// The number of words. A record that was read has an even number, because the encoding pads.
    pub fn len(&self) -> usize {
        usize::from(self.len)
    }

    /// True when the record has no words.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The words.
    pub fn words(&self) -> &[u32] {
        &self.words[..self.len()]
    }

    /// The word `i`, or 0 past the end.
    pub fn word(&self, i: usize) -> u32 {
        self.words.get(i).copied().unwrap_or(0)
    }

    /// The 64-bit value at the words `i` and `i + 1`.
    pub fn word64(&self, i: usize) -> u64 {
        u64::from(self.word(i)) | u64::from(self.word(i + 1)) << 32
    }

    /// The payload size in bytes, padded to a multiple of 8.
    pub(crate) fn payload_bytes(&self) -> Result<usize> {
        if self.over {
            return Err(NvnError::FieldRange("extension record words"));
        }
        Ok(self.len().div_ceil(2) * 8)
    }

    /// Writes the whole record, header and words, to the start of `out`. Gives the size.
    pub(crate) fn encode_into(&self, out: &mut [u8]) -> Result<usize> {
        let size = HEADER_BYTES as usize + self.payload_bytes()?;
        let out = out
            .get_mut(..size)
            .ok_or(NvnError::FieldRange("extension record size"))?;
        write_ext(out, self.code, self.arg, self.words());
        Ok(size)
    }

    /// An extension from a header and its payload bytes.
    pub(crate) fn from_payload(code: ExtCode, arg: u16, body: &[u8]) -> Self {
        let mut e = Self::new(code, arg);
        for c in body.chunks_exact(4) {
            e = e.u32(u32::from_le_bytes([c[0], c[1], c[2], c[3]]));
        }
        e
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_is_found_by_its_number() {
        for (i, c) in ExtCode::all().iter().enumerate() {
            assert_eq!(*c as u16, FIRST_EXT_OP + i as u16);
            assert_eq!(ExtCode::from_u16(*c as u16), Some(*c));
        }
        assert_eq!(ExtCode::from_u16(63), None);
        assert_eq!(ExtCode::from_u16(FIRST_EXT_OP + CODES.len() as u16), None);
    }

    #[test]
    fn write_ext_matches_the_size_and_the_words() {
        assert_eq!(ext_size(0), 8);
        assert_eq!(ext_size(3), 16);
        let mut out = [0xffu8; 16];
        write_ext(&mut out, ExtCode::Draw, 7, &[1, 2, 3]);
        assert_eq!(&out[0..2], &(ExtCode::Draw as u16).to_le_bytes());
        assert_eq!(&out[2..4], &7u16.to_le_bytes());
        assert_eq!(&out[4..8], &16u32.to_le_bytes());
        assert_eq!(&out[8..12], &1u32.to_le_bytes());
        assert_eq!(&out[16 - 4..], &[0; 4]);
    }

    #[test]
    fn values_go_in_as_low_word_first() {
        let e = Ext::new(ExtCode::Program, 0)
            .u64(0x1122_3344_5566_7788)
            .i32(-1)
            .f32(1.0);
        assert_eq!(e.words(), &[0x5566_7788, 0x1122_3344, u32::MAX, 0x3f80_0000]);
        assert_eq!(e.word64(0), 0x1122_3344_5566_7788);
        assert_eq!(e.word(9), 0);
    }

    #[test]
    fn bytes_are_padded_to_a_word() {
        let e = Ext::new(ExtCode::Push, 0).bytes(&[1, 2, 3, 4, 5]);
        assert_eq!(e.words(), &[0x0403_0201, 5]);
    }

    #[test]
    fn too_many_words_fail_at_encoding() {
        let mut e = Ext::new(ExtCode::Push, 0);
        for i in 0..=MAX_EXT_WORDS {
            e = e.u32(i as u32);
        }
        assert!(e.payload_bytes().is_err());
        let ok = Ext::new(ExtCode::Push, 0).bytes(&[0; MAX_EXT_WORDS * 4]);
        assert_eq!(ok.payload_bytes().unwrap(), MAX_EXT_WORDS * 4);
    }
}
