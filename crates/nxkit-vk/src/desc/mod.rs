//! The backend-neutral descriptions of the objects: what the backend gets to make its own objects from.
//!
//! The ids are the Vulkan handles of the objects ([`crate::handle`]), so they never collide across kinds and are never
//! reused. Enum values that have no meaning to this crate (blend factors, compare ops, filters, address modes) stay as
//! the raw Vulkan values; the backend maps them. Nothing here is a Metal type.

mod pipeline;
mod resource;

pub use pipeline::*;
pub use resource::*;

/// The storage of a `VkDeviceMemory`. The id 0 means no memory (a buffer that was never bound); the backend skips
/// what refers to it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MemoryId(pub u64);

/// A texture of the backend: an image, an image view, a texel buffer view or a swapchain image.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TextureId(pub u64);

/// A sampler of the backend.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SamplerId(pub u64);

/// A pipeline of the backend.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PipelineId(pub u64);

/// A query pool of the backend.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct QueryPoolId(pub u64);

/// The key of a shader module: the first 8 bytes of the SHA-256 of its SPIR-V (`pCode`). The offline translation of
/// the shaders names each Metal function after it (`vk_<16 hex digits>`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ShaderKey(pub [u8; 8]);

impl ShaderKey {
    /// The key of a SPIR-V module.
    pub fn of(code: &[u8]) -> Self {
        let d = nxb_model::sha256(code);
        let mut k = [0u8; 8];
        k.copy_from_slice(&d.0[..8]);
        Self(k)
    }

    /// The 16 lower-case hex digits of the key.
    pub fn to_hex(self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// A range of a buffer, resolved to its memory: `offset` counts from the start of the memory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct BufferRef {
    /// The memory the buffer is bound to; [`MemoryId`] 0 when it is not bound.
    pub memory: MemoryId,
    /// The first byte, from the start of the memory.
    pub offset: u64,
    /// The bytes from `offset` to the end of the range.
    pub size: u64,
}

/// An image as a command names it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ImageRef {
    /// The texture of the image.
    pub texture: TextureId,
    /// The `VkFormat` of the image.
    pub format: u32,
    /// The samples per texel.
    pub samples: u32,
}

/// A 2D rectangle (`VkRect2D`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rect2D {
    /// The left edge.
    pub x: i32,
    /// The top edge.
    pub y: i32,
    /// The width.
    pub width: u32,
    /// The height.
    pub height: u32,
}

/// A viewport (`VkViewport`). The shaders flip y, so it stays as the game gave it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Viewport {
    /// `x`.
    pub x: f32,
    /// `y`.
    pub y: f32,
    /// `width`.
    pub width: f32,
    /// `height`.
    pub height: f32,
    /// `minDepth`.
    pub min_depth: f32,
    /// `maxDepth`.
    pub max_depth: f32,
}

/// The type of a texture, as the backend makes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextureKind {
    /// A 2D texture. A 1D image is a one-row 2D texture: Metal's 1D texture is no render target, has one level and no
    /// depth format, all of which Vulkan's 1D image allows (Hades draws into a 512x1 colour target).
    D2,
    /// A 2D array texture (also a 1D array image).
    D2Array,
    /// A multisample 2D texture.
    D2Multisample,
    /// A multisample 2D array texture.
    D2MultisampleArray,
    /// A cube texture.
    Cube,
    /// A cube array texture.
    CubeArray,
    /// A 3D texture.
    D3,
    /// A texture over a buffer (a texel buffer view).
    Buffer,
}

/// A component of a view swizzle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Swizzle {
    /// Zero.
    Zero,
    /// One.
    One,
    /// Red.
    R,
    /// Green.
    G,
    /// Blue.
    B,
    /// Alpha.
    A,
}

impl Swizzle {
    /// `VkComponentSwizzle`, with `IDENTITY` as `own`.
    pub fn from_vk(v: u32, own: Swizzle) -> Self {
        match v {
            1 => Self::Zero,
            2 => Self::One,
            3 => Self::R,
            4 => Self::G,
            5 => Self::B,
            6 => Self::A,
            _ => own,
        }
    }
}

/// The identity swizzle.
pub const IDENTITY_SWIZZLE: [Swizzle; 4] = [Swizzle::R, Swizzle::G, Swizzle::B, Swizzle::A];

/// What a descriptor set element holds. It holds the backend objects that it was written with, as a hardware descriptor
/// holds a copy of the object's handle: a set may still name a view, sampler or buffer that the game destroyed. The
/// backend binds nothing for an id that it no longer knows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Descriptor {
    /// Not written.
    #[default]
    Empty,
    /// A sampler.
    Sampler(SamplerId),
    /// An image view, with its sampler for a combined image sampler.
    Image {
        /// The view.
        view: TextureId,
        /// The sampler, for `COMBINED_IMAGE_SAMPLER`.
        sampler: Option<SamplerId>,
    },
    /// A uniform or storage buffer range (dynamic ones get their dynamic offset at bind).
    Buffer(BufferRef),
    /// A texel buffer view.
    TexelBuffer(TextureId),
}

/// The kind of a query pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum QueryType {
    /// `VK_QUERY_TYPE_OCCLUSION`.
    Occlusion,
    /// `VK_QUERY_TYPE_TIMESTAMP`: the completion time of the submit in ns (`timestampPeriod` 1).
    Timestamp,
    /// `VK_QUERY_TYPE_PIPELINE_STATISTICS` and others: never written, reported once.
    Other(u32),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shader_key_is_the_head_of_the_sha256() {
        // sha256("abc") = ba7816bf8f01cfea...
        let k = ShaderKey::of(b"abc");
        assert_eq!(k.to_hex(), "ba7816bf8f01cfea");
    }

    #[test]
    fn swizzles_keep_their_own_component_for_identity() {
        assert_eq!(Swizzle::from_vk(0, Swizzle::G), Swizzle::G);
        assert_eq!(Swizzle::from_vk(2, Swizzle::G), Swizzle::One);
        assert_eq!(Swizzle::from_vk(6, Swizzle::R), Swizzle::A);
    }
}
