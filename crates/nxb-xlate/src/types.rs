//! Enums and values of the translator that the driver queries and prints, and the two dispatch offsets of c0 that the
//! emitter and the MSL back end share.

/// `SamplerType` flags: the low byte is the dimension, bits 8 to 10 are flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SamplerType(pub u32);

impl SamplerType {
    pub const NONE: SamplerType = SamplerType(0);
    pub const TEXTURE_1D: SamplerType = SamplerType(1);
    pub const TEXTURE_BUFFER: SamplerType = SamplerType(2);
    pub const TEXTURE_2D: SamplerType = SamplerType(3);
    pub const TEXTURE_3D: SamplerType = SamplerType(4);
    pub const TEXTURE_CUBE: SamplerType = SamplerType(5);
    pub const MASK: u32 = 0xff;
    pub const ARRAY: u32 = 1 << 8;
    pub const MULTISAMPLE: u32 = 1 << 9;
    pub const SHADOW: u32 = 1 << 10;

    /// The same type with extra flag bits set.
    pub fn with(self, flags: u32) -> SamplerType {
        SamplerType(self.0 | flags)
    }

    /// `type & mask`.
    pub fn and(self, mask: u32) -> SamplerType {
        SamplerType(self.0 & mask)
    }

    /// `Enum.ToString()` of the `[Flags]` enum, as the notes print it.
    pub fn name(self) -> String {
        const NAMES: [(u32, &str); 9] = [
            (0x400, "Shadow"),
            (0x200, "Multisample"),
            (0x100, "Array"),
            (0xff, "Mask"),
            (5, "TextureCube"),
            (4, "Texture3D"),
            (3, "Texture2D"),
            (2, "TextureBuffer"),
            (1, "Texture1D"),
        ];
        if self.0 == 0 {
            return "None".into();
        }
        let mut rest = self.0;
        let mut parts: Vec<&str> = Vec::new();
        for (v, n) in NAMES {
            // .NET matches a member when all its bits are set, multi-bit members must match fully
            if rest & v == v && v != 0 {
                if v < 0x100 && v != 0xff && (rest & 0xff) != v {
                    continue;
                }
                parts.push(n);
                rest &= !v;
            }
        }
        if rest != 0 {
            return self.0.to_string();
        }
        parts.reverse();
        parts.join(", ")
    }
}

/// `TextureFormat` in declaration order (`Enum.ToString()` gives the name).
pub const TEXTURE_FORMATS: [&str; 40] = [
    "Unknown",
    "R8Unorm",
    "R8Snorm",
    "R8Uint",
    "R8Sint",
    "R16Float",
    "R16Unorm",
    "R16Snorm",
    "R16Uint",
    "R16Sint",
    "R32Float",
    "R32Uint",
    "R32Sint",
    "R8G8Unorm",
    "R8G8Snorm",
    "R8G8Uint",
    "R8G8Sint",
    "R16G16Float",
    "R16G16Unorm",
    "R16G16Snorm",
    "R16G16Uint",
    "R16G16Sint",
    "R32G32Float",
    "R32G32Uint",
    "R32G32Sint",
    "R8G8B8A8Unorm",
    "R8G8B8A8Snorm",
    "R8G8B8A8Uint",
    "R8G8B8A8Sint",
    "R16G16B16A16Float",
    "R16G16B16A16Unorm",
    "R16G16B16A16Snorm",
    "R16G16B16A16Uint",
    "R16G16B16A16Sint",
    "R32G32B32A32Float",
    "R32G32B32A32Uint",
    "R32G32B32A32Sint",
    "R10G10B10A2Unorm",
    "R10G10B10A2Uint",
    "R11G11B10Float",
];

/// Index of a format name.`Enum.TryParse(ignoreCase)`.
pub fn parse_format(s: &str) -> Option<usize> {
    TEXTURE_FORMATS
        .iter()
        .position(|n| n.eq_ignore_ascii_case(s))
}

/// Attribute and fragment output type of the graphics state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttributeType {
    Float,
    Sint,
    Uint,
}

/// The kind of a texture handle (bits 28 and up of the packed handle).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandleType {
    CombinedSampler,
    SeparateSamplerHandle,
    SeparateSamplerId,
    SeparateConstantSamplerHandle,
    Direct,
    Other(u32),
}

/// `TextureHandle.UnpackOffsets`: word offsets 0 and 1 and the type.
pub fn unpack_offsets(handle: i32) -> (i32, i32, HandleType) {
    let t = match (handle as u32) >> 28 {
        0 => HandleType::CombinedSampler,
        1 => HandleType::SeparateSamplerHandle,
        2 => HandleType::SeparateSamplerId,
        3 => HandleType::SeparateConstantSamplerHandle,
        4 => HandleType::Direct,
        n => HandleType::Other(n),
    };
    (handle & 0x3fff, (handle >> 14) & 0x3fff, t)
}

/// Compute (ABI of the MSL libraries, `msl::abi`): the dispatch's workgroup counts in c0.
pub const C0_DISPATCH: u32 = 0x200;
/// NVN2 compute: the workgroup counts in c0.
pub const C0_DISPATCH_NVN2: u32 = 4;
