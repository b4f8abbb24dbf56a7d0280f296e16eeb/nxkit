//! NVN texture formats: bytes per block, block size and which aspects the format holds.
//!
//! The ranges follow `nvn_fmt_bpb` (see `THIRD_PARTY.md`). The numbers are the values of `NVNformat`.

use crate::error::{NvnError, Result};

/// What a format stores.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aspects {
    /// Colour data.
    Color,
    /// Depth only.
    Depth,
    /// Stencil only.
    Stencil,
    /// Depth and stencil.
    DepthStencil,
}

impl Aspects {
    /// True when the format has a depth part.
    pub const fn has_depth(self) -> bool {
        matches!(self, Self::Depth | Self::DepthStencil)
    }

    /// True when the format has a stencil part.
    pub const fn has_stencil(self) -> bool {
        matches!(self, Self::Stencil | Self::DepthStencil)
    }
}

/// The storage of one format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FormatInfo {
    /// The bytes of one block. A block is one texel for formats that are not compressed.
    pub bytes_per_block: u8,
    /// The width of a block in texels.
    pub block_w: u8,
    /// The height of a block in texels.
    pub block_h: u8,
    /// The aspects of the format.
    pub aspects: Aspects,
}

/// `NVN_FORMAT_STENCIL8`.
pub const FORMAT_STENCIL8: i32 = 0x31;
/// `NVN_FORMAT_DEPTH16`.
pub const FORMAT_DEPTH16: i32 = 0x32;
/// `NVN_FORMAT_DEPTH24_STENCIL8`.
pub const FORMAT_DEPTH24_STENCIL8: i32 = 0x35;
/// `NVN_FORMAT_DEPTH32F_STENCIL8`.
pub const FORMAT_DEPTH32F_STENCIL8: i32 = 0x36;

/// The ASTC block sizes in the order of the NVN formats.
const ASTC: [(u8, u8); 14] = [
    (4, 4),
    (5, 4),
    (5, 5),
    (6, 5),
    (6, 6),
    (8, 5),
    (8, 6),
    (8, 8),
    (10, 5),
    (10, 6),
    (10, 8),
    (10, 10),
    (12, 10),
    (12, 12),
];

/// First format, last format and bytes per texel of the formats that are not compressed.
const PLAIN: [(i32, i32, u8); 37] = [
    (0x01, 0x04, 1),
    (0x05, 0x09, 2),
    (0x0A, 0x0C, 4),
    (0x0D, 0x10, 2),
    (0x11, 0x15, 4),
    (0x16, 0x18, 8),
    (0x19, 0x1C, 3),
    (0x1D, 0x21, 6),
    (0x22, 0x24, 12),
    (0x25, 0x28, 4),
    (0x29, 0x2D, 8),
    (0x2E, 0x30, 16),
    (0x31, 0x31, 1),
    (0x32, 0x32, 2),
    (0x33, 0x35, 4),
    (0x36, 0x36, 8),
    (0x37, 0x38, 4),
    (0x39, 0x3C, 2),
    (0x3D, 0x40, 4),
    (0x51, 0x52, 1),
    (0x53, 0x54, 2),
    (0x55, 0x56, 4),
    (0x57, 0x58, 2),
    (0x59, 0x5A, 4),
    (0x5B, 0x5C, 8),
    (0x5D, 0x5E, 3),
    (0x5F, 0x60, 6),
    (0x61, 0x62, 12),
    (0x63, 0x64, 4),
    (0x65, 0x66, 8),
    (0x67, 0x68, 16),
    (0x69, 0x6C, 4),
    (0x6D, 0x70, 4),
    (0x71, 0x75, 8),
    (0x76, 0x78, 16),
    (0x95, 0x98, 2),
    (0x99, 0x9C, 4),
];

/// The storage of an NVN format.
pub fn format_info(format: i32) -> Result<FormatInfo> {
    let (bytes_per_block, block_w, block_h) = match format {
        0x79..=0x94 => {
            let (w, h) = ASTC[(format as usize - 0x79) % 14];
            (16, w, h)
        }
        0x41..=0x50 => {
            let small = format <= 0x42 || matches!(format, 0x45 | 0x46 | 0x49 | 0x4A);
            (if small { 8 } else { 16 }, 4, 4)
        }
        _ => {
            let row = PLAIN.iter().find(|r| (r.0..=r.1).contains(&format));
            (row.ok_or(NvnError::UnknownFormat(format))?.2, 1, 1)
        }
    };
    let aspects = match format {
        FORMAT_STENCIL8 => Aspects::Stencil,
        0x32..=0x34 => Aspects::Depth,
        FORMAT_DEPTH24_STENCIL8 | FORMAT_DEPTH32F_STENCIL8 => Aspects::DepthStencil,
        _ => Aspects::Color,
    };
    Ok(FormatInfo {
        bytes_per_block,
        block_w,
        block_h,
        aspects,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_of_common_formats() {
        let rgba8 = format_info(0x25).unwrap();
        assert_eq!((rgba8.bytes_per_block, rgba8.block_w), (4, 1));
        let bc1 = format_info(0x41).unwrap();
        assert_eq!((bc1.bytes_per_block, bc1.block_w, bc1.block_h), (8, 4, 4));
        let bc7 = format_info(0x50).unwrap();
        assert_eq!(bc7.bytes_per_block, 16);
        let astc6x6 = format_info(0x79 + 4).unwrap();
        assert_eq!((astc6x6.block_w, astc6x6.block_h), (6, 6));
    }

    #[test]
    fn depth_and_stencil_aspects() {
        assert_eq!(format_info(FORMAT_DEPTH16).unwrap().aspects, Aspects::Depth);
        assert!(
            format_info(FORMAT_DEPTH24_STENCIL8)
                .unwrap()
                .aspects
                .has_stencil()
        );
        assert!(
            format_info(FORMAT_DEPTH32F_STENCIL8)
                .unwrap()
                .aspects
                .has_depth()
        );
        assert!(!format_info(0x25).unwrap().aspects.has_depth());
        assert_eq!(
            format_info(FORMAT_STENCIL8).unwrap().aspects,
            Aspects::Stencil
        );
    }

    #[test]
    fn unknown_formats_are_errors() {
        assert_eq!(format_info(0), Err(NvnError::UnknownFormat(0)));
        assert!(format_info(0x4000).is_err());
    }
}
